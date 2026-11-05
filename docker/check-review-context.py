import argparse
import contextlib
import json
import pathlib
import secrets
import subprocess
import tempfile
import time
import uuid


def command(*args, input=None, check=True, timeout=120):
    result = subprocess.run(args, input=input, capture_output=True, text=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} {args[1]} failed: {result.stderr.strip()}")
    return result


def marked_json(text, marker):
    if marker not in text:
        raise RuntimeError("Diagnostic did not produce the expected result")
    return json.loads(text.split(marker, 1)[1].splitlines()[0])


def volume_sockets(image, volume, target):
    # Runner replicas share one IPC volume; each listens on /ipc/<its hostname>.sock.
    listing = command("docker", "run", "--rm", "--network", "none", "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true", "--entrypoint", "/bin/sh", "--mount", f"type=volume,source={volume},target=/ipc,readonly", image, "-c", 'cd /ipc && for socket in *.sock; do if test -S "$socket"; then echo "$socket"; fi; done').stdout.split()
    return [f"{target}/{name}" for name in sorted(listing)]


POSTGRES_IMAGE = "postgres:17@sha256:ae69c452f483507a6b99fb654cf93aad7fe156ffd2c56247707eef4e36d3c12b"


@contextlib.contextmanager
def scratch_database():
    # Context preparation writes the head commit's index to a database. A throwaway Postgres on an
    # internal network gives it one while the diagnostic stays without any outside network.
    prefix = "ocr-context-db-" + uuid.uuid4().hex[:12]
    network, database = prefix + "-net", prefix + "-pg"
    password = secrets.token_hex(16)
    created_network = created_database = False
    try:
        command("docker", "network", "create", "--internal", network)
        created_network = True
        command("docker", "run", "-d", "--name", database, "--network", network, "--tmpfs", "/var/lib/postgresql/data", "-e", "POSTGRES_PASSWORD=" + password, POSTGRES_IMAGE, timeout=300)
        created_database = True
        # The image's init server listens only on its socket; TCP answers once the real one is up.
        for _ in range(150):
            if command("docker", "exec", database, "pg_isready", "-h", "127.0.0.1", "-U", "postgres", check=False).returncode == 0:
                break
            time.sleep(0.2)
        else:
            raise RuntimeError("Diagnostic database did not become ready")
        yield network, f"postgres://postgres:{password}@{database}:5432/postgres"
    finally:
        if created_database:
            command("docker", "rm", "-f", "-v", database, check=False)
        if created_network:
            command("docker", "network", "rm", network, check=False)


@contextlib.contextmanager
def isolated_runner(image, container):
    prefix = "ocr-cli-preflight-" + uuid.uuid4().hex[:12]
    containers, networks, volumes = [], [], []
    mounts = json.loads(command("docker", "inspect", "--format", "{{json .Mounts}}", container).stdout)
    auth = next((mount["Name"] for mount in mounts if mount["Destination"] == "/review-claude-auth" and mount["Type"] == "volume"), None)
    if not auth:
        raise RuntimeError("App-owned runner authentication volume unavailable")
    try:
        outbound, internal, ipc = prefix + "-outbound", prefix + "-internal", prefix + "-ipc"
        for name, flags in [(outbound, []), (internal, ["--internal"])]:
            command("docker", "network", "create", *flags, name)
            networks.append(name)
        command("docker", "volume", "create", ipc)
        volumes.append(ipc)
        common = ["--init", "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true", "--pids-limit", "128", "--tmpfs", "/data:size=1m,mode=0700", "--tmpfs", "/tmp:size=64m,mode=1777"]
        proxy = prefix + "-proxy"
        command("docker", "create", "--name", proxy, *common, "--network", outbound, "--user", "10001:10001", "--cpus", "0.25", "--memory", "128m", image, "egress")
        containers.append(proxy)
        command("docker", "network", "connect", "--alias", "cli-preflight-egress", internal, proxy)
        command("docker", "start", proxy)
        runner = prefix + "-runner"
        command("docker", "create", "--name", runner, "--hostname", runner, *common, "--network", internal, "--cpus", "1", "--memory", "2g", "--memory-swap", "2g", "--cap-add", "SETUID", "--cap-add", "SETGID", "--cap-add", "CHOWN", "--cap-add", "DAC_OVERRIDE", "--cap-add", "KILL", "--mount", f"type=volume,source={ipc},target=/ipc", "--mount", f"type=volume,source={auth},target=/claude-auth,readonly", "-e", "OCR_RUNNER_CLAUDE_TOKEN=/claude-auth/app-managed/claude/oauth-token", "-e", "OCR_RUNNER_PROXY=http://cli-preflight-egress:8790", "--tmpfs", "/work:size=16m,mode=0755", "--tmpfs", "/evidence:size=512m,mode=0755", "--tmpfs", "/output:size=48m,mode=0777", "--tmpfs", "/scratch:size=128m,mode=0755", image, "runner")
        containers.append(runner)
        command("docker", "start", runner)
        for _ in range(30):
            if command("docker", "exec", runner, "test", "-S", f"/ipc/{runner}.sock", check=False).returncode == 0:
                break
            time.sleep(0.2)
        else:
            raise RuntimeError("Disposable runner did not become ready")
        print(json.dumps({"disposableRunner": runner, "image": image, "modelCalls": 0}), flush=True)
        yield ipc
    finally:
        for name in reversed(containers):
            command("docker", "stop", "--timeout", "2", name, check=False)
            command("docker", "rm", "-v", name, check=False)
        for name in reversed(networks):
            command("docker", "network", "rm", name, check=False)
        for name in reversed(volumes):
            command("docker", "volume", "rm", name, check=False)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--container", default="opencodereview-server-1")
    parser.add_argument("--volume", default="opencodereview_ocr-data")
    parser.add_argument("--repository", required=True)
    parser.add_argument("--pr", type=int, required=True)
    parser.add_argument("--run-id", type=int)
    parser.add_argument("--skill-json")
    parser.add_argument("--image", default="ocr-review-tests:local")
    parser.add_argument("--runner-ipc-volume", action="append", default=[])
    parser.add_argument("--runner-image")
    parser.add_argument("--preflight-model")
    parser.add_argument("--pinned-dependencies", action="store_true")
    parser.add_argument("--hydrate-pinned-dependencies", action="store_true")
    replan = parser.add_mutually_exclusive_group()
    replan.add_argument("--replan-publication-scope", action="store_true")
    replan.add_argument("--replan-incremental-scope", action="store_true")
    replan.add_argument("--replan-base-sync-scope", action="store_true")
    replan.add_argument("--reprocess-scope", action="store_true")
    export = parser.add_mutually_exclusive_group()
    export.add_argument("--export-chunk", type=int)
    export.add_argument("--export-all-chunks", action="store_true")
    parser.add_argument("--export-directory")
    args = parser.parse_args()
    if args.hydrate_pinned_dependencies and not args.pinned_dependencies:
        parser.error("Dependency hydration requires --pinned-dependencies")
    if args.runner_image and args.runner_ipc_volume:
        parser.error("Choose a disposable runner image or existing runner IPC volumes")
    exporting = args.export_chunk is not None or args.export_all_chunks
    if args.preflight_model and (exporting or not (args.runner_image or args.runner_ipc_volume)):
        parser.error("A model override is limited to non-exporting runtime preflight")
    if exporting != (args.export_directory is not None):
        parser.error("Export requires an explicit selection and a private directory")
    export_args = []
    if exporting:
        if args.export_chunk is not None and args.export_chunk < 1:
            parser.error("Chunk must be positive")
        directory = pathlib.Path(args.export_directory).resolve()
        directory.mkdir(mode=0o700)
        export_args = ["--mount", f"type=bind,source={directory},target=/diagnostic", "-e", "OCR_DIAGNOSTIC_EXPORT_STAGE=/diagnostic", "-e", "OCR_DIAGNOSTIC_EXPORT_ALL=true" if args.export_all_chunks else "OCR_DIAGNOSTIC_CHUNK=" + str(args.export_chunk)]
    if len(args.runner_ipc_volume) > 3:
        parser.error("At most three runner IPC volumes are supported")
    identifier = "ocr-context-check-" + uuid.uuid4().hex[:16]
    installed = "/tmp/" + identifier
    created = copied = False
    resources = contextlib.ExitStack()
    try:
        if args.runner_image:
            args.runner_ipc_volume = [resources.enter_context(isolated_runner(args.runner_image, args.container))]
        with tempfile.TemporaryDirectory(prefix=identifier) as temporary:
            binary = pathlib.Path(temporary) / "ocr-review-tests"
            command("docker", "create", "--name", identifier, args.image)
            created = True
            command("docker", "cp", identifier + ":/usr/local/bin/ocr-review-tests", str(binary))
            command("docker", "exec", args.container, "test", "!", "-e", installed)
            copied = True
            command("docker", "cp", str(binary), args.container + ":" + installed)
            run_env = ["-e", "OCR_DIAGNOSTIC_RUN=" + str(args.run_id)] if args.run_id is not None else []
            if args.replan_base_sync_scope:
                run_env.extend(["-e", "OCR_DIAGNOSTIC_REPLAN_BASE_SYNC=true"])
            if args.reprocess_scope:
                run_env.extend(["-e", "OCR_DIAGNOSTIC_REPROCESS=true"])
            export = command("docker", "exec", *run_env, "-e", "OCR_DIAGNOSTIC_REPOSITORY=" + args.repository, "-e", "OCR_DIAGNOSTIC_PR=" + str(args.pr), args.container, installed, "review::usage_tests::export_read_only_review_context_fixture", "--exact", "--ignored", "--nocapture", "--test-threads=1")
            fixture = marked_json(export.stdout, "READ_ONLY_REVIEW_FIXTURE ")
            if args.preflight_model:
                print(json.dumps({"recordedModel": fixture.get("model"), "preflightModel": args.preflight_model, "modelCalls": 0}), flush=True)
                fixture["model"] = args.preflight_model
            if args.skill_json:
                fixture["skill"] = json.loads(pathlib.Path(args.skill_json).read_text())["content"]
            print(json.dumps({"exportedRun": fixture["runId"], "changedFiles": len(fixture["plan"]["changedFiles"]), "relatedFiles": len(fixture["plan"]["relatedFiles"]), "databaseWrites": False}), flush=True)
            project = args.repository.replace("/", "%2F")
            mr = json.loads(command(
                "docker", "exec", args.container, "glab", "api",
                f"projects/{project}/merge_requests/{args.pr}",
            ).stdout)
            head = mr.get("sha") or (mr.get("diff_refs") or {}).get("head_sha")
            if not head:
                raise RuntimeError("GitLab merge request response missing head sha")
            pr = {"head": head, "title": mr.get("title") or "", "body": mr.get("description") or ""}
            packet = json.dumps({"fixture": fixture, "pr": pr})
            if args.hydrate_pinned_dependencies:
                hydration = command("docker", "exec", "-i", *run_env, "-e", "OCR_DIAGNOSTIC_REPOS=/data/repos", args.container, installed, "review::usage_tests::hydrate_pinned_review_dependencies_without_inference", "--exact", "--ignored", "--nocapture", "--test-threads=1", input=packet, timeout=600)
                hydration_result = marked_json(hydration.stdout, "PINNED_DEPENDENCY_HYDRATION ")
                print(json.dumps(hydration_result), flush=True)
                if "authorizedDependencies" in hydration_result:
                    fixture["authorizedDependencies"] = hydration_result["authorizedDependencies"]
                if "recordedDependencies" in hydration_result:
                    fixture["recordedDependencies"] = hydration_result["recordedDependencies"]
                    packet = json.dumps({"fixture": fixture, "pr": pr})
            elif fixture.get("recordedDependencies"):
                print(json.dumps({"runId": fixture["runId"], "reconstructedFromArchive": True, "recordedDependencies": len(fixture["recordedDependencies"]), "modelCalls": 0}), flush=True)
            runner_args = ["-e", "OCR_DIAGNOSTIC_REPLAN_PUBLICATION=true"] if args.replan_publication_scope else []
            if args.replan_incremental_scope:
                runner_args.extend(["-e", "OCR_DIAGNOSTIC_REPLAN_INCREMENTAL=true"])
            if args.replan_base_sync_scope:
                runner_args.extend(["-e", "OCR_DIAGNOSTIC_REPLAN_BASE_SYNC=true"])
            if args.reprocess_scope:
                runner_args.extend(["-e", "OCR_DIAGNOSTIC_REPROCESS=true"])
            if args.pinned_dependencies:
                runner_args.extend(["-e", "OCR_DIAGNOSTIC_PINNED_DEPENDENCIES=true"])
            sockets = []
            for index, volume in enumerate(args.runner_ipc_volume):
                runner_args.extend(["--mount", f"type=volume,source={volume},target=/runner-ipc-{index},readonly"])
                sockets.extend(volume_sockets(args.image, volume, f"/runner-ipc-{index}"))
            if args.runner_ipc_volume and not sockets:
                raise RuntimeError("No runner socket found in the runner IPC volumes")
            if len(sockets) > 3:
                print(json.dumps({"runnerSockets": len(sockets), "preflightedSockets": sockets[:3]}), flush=True)
                sockets = sockets[:3]
            if sockets:
                runner_args.extend(["-e", "OCR_DIAGNOSTIC_RUNNER_SOCKETS=" + ",".join(sockets)])
            database_network, database_url = resources.enter_context(scratch_database())
            result = command("docker", "run", "--rm", "-i", "--init", "--network", database_network, "-e", "OCR_TEST_DATABASE_URL=" + database_url, "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true", "--pids-limit", "128", "--cpus", "1", "--memory", "768m", "--memory-swap", "768m", "--tmpfs", "/tmp:size=128m", "--mount", f"type=volume,source={args.volume},volume-subpath=repos,target=/repos,readonly", "-e", "OCR_DIAGNOSTIC_REPOS=/repos", "-e", "GIT_NO_LAZY_FETCH=1", "-e", "OCR_GITLAB_HOST=gitlab.example.com", *runner_args, *export_args, args.image, "review::usage_tests::persisted_review_chunk_preparation_without_inference", "--exact", "--ignored", "--nocapture", "--test-threads=1", input=packet, check=False, timeout=1200 if sockets else 180)
            for line in result.stdout.splitlines():
                for marker in ["OFFLINE_ARCHIVE_DIAGNOSTIC ", "PINNED_DEPENDENCY_DIAGNOSTIC ", "OFFLINE_SCOPE_REPLAN ", "RUNTIME_PREFLIGHT ", "EXPORTED_DIAGNOSTIC_STAGE "]:
                    if marker in line:
                        print(json.dumps(marked_json(line, marker)), flush=True)
            if result.returncode:
                for line in result.stdout.splitlines():
                    if "OFFLINE_CONTEXT_DIAGNOSTIC " in line:
                        print(json.dumps(marked_json(line, "OFFLINE_CONTEXT_DIAGNOSTIC "), indent=2), flush=True)
                raise RuntimeError("Offline preparation failed: " + result.stderr.strip())
            report = marked_json(result.stdout, "OFFLINE_CHUNK_PREPARATION ")
            if exporting:
                report["chunkCount"] = len(report["chunks"])
                if args.export_chunk is not None:
                    report["chunks"] = [chunk for chunk in report["chunks"] if chunk["chunk"] == args.export_chunk]
                report["exportDirectory"] = str(directory)
            print(json.dumps(report, indent=2), flush=True)
    finally:
        try:
            if copied:
                command("docker", "exec", args.container, "rm", "--", installed, check=False)
            if created:
                command("docker", "rm", "-v", identifier, check=False)
        finally:
            resources.close()


if __name__ == "__main__":
    main()
