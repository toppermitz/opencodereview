import base64
import concurrent.futures
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import time
import uuid

IMAGE = os.environ.get("OCR_SMOKE_IMAGE", "opencodereview-runner:local")
MODEL = "claude-opus-5-5"
ENGINE_VERSION = ""
PREFIX = "ocr-runner-smoke-" + uuid.uuid4().hex[:10]
containers, networks, volumes = [], [], []
sockets = {}


def docker(*args, input=None, check=True, timeout=45):
    result = subprocess.run(["docker", *args], input=input, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"docker {args[0]} failed: {result.stderr.strip()}")
    return result


def start(name, args):
    docker("create", "--name", name, "--init", "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges:true", "--pids-limit=64", "--memory=512m", "--memory-swap=512m", "--cpus=0.5", "--tmpfs", "/data:size=1m,mode=0700", *args)
    containers.append(name)
    docker("start", name)


CLIENT = r'''
use strict;
use IO::Socket::UNIX;
use JSON::PP;
use Time::HiRes qw(time sleep);
local $/;
my $packet = decode_json(<STDIN>);
my $request = encode_json($packet->{request});
my $socket = IO::Socket::UNIX->new(Type => SOCK_STREAM, Peer => $ENV{SMOKE_SOCKET}) or die "IPC connect failed";
$socket->autoflush(1);
sub exact {
    my ($length) = @_;
    my $data = '';
    while (length($data) < $length) {
        my $count = sysread($socket, my $part, $length - length($data));
        die "Truncated IPC" unless $count;
        $data .= $part;
    }
    return $data;
}
for my $frame (@{$packet->{frames} || []}) {
    my $encoded = encode_json($frame);
    print $socket pack('N', length($encoded)) . $encoded;
    my $length = unpack('N', exact(4));
    die "Oversized acknowledgement" if $length > 65536;
    my $ack = decode_json(exact($length));
    die encode_json($ack) unless $ack->{event} eq 'Ack';
}
print $socket pack('N', length($request)) . $request;
while (1) {
    my $length = unpack('N', exact(4));
    die "Oversized IPC" if $length > 48 * 1024 * 1024;
    my $value = decode_json(exact($length));
    $value->{receivedAt} = time;
    print encode_json($value) . "\n";
    if ($value->{event} eq 'Started' && $ENV{SMOKE_CONTROL}) {
        sleep(0.3);
        last if $ENV{SMOKE_CONTROL} eq 'disconnect';
        print $socket chr(1);
    }
    last if $value->{event} eq 'Finished' || $value->{event} eq 'Error' || $value->{event} eq 'Catalog' || $value->{event} eq 'Ready';
}
'''

FAKE = r'''#!/bin/sh
set -eu
model=""
settings=""
for arg in "$@"; do
  if [ "$arg" = --version ]; then printf '0.0.1 (Claude Code)\n'; exit 0; fi
done
while [ "$#" -gt 0 ]; do
  case "$1" in
    --model) model="$2"; shift ;;
    --settings) test -z "$settings"; settings="$2"; shift ;;
    --tools) test "$2" = Read,Grep,Glob,Bash,Write; shift ;;
    --permission-mode) test "$2" = dontAsk; shift ;;
  esac
  shift
done
test "$settings" = /scratch/session/settings.json
test ! -w "$settings"
test ! -w /scratch/session
test "$CLAUDE_CONFIG_DIR" = /scratch/session/.claude
test -w "$CLAUDE_CONFIG_DIR"
test "$CLAUDE_CODE_OAUTH_TOKEN" = fixture-not-a-real-credential
test "$DISABLE_TELEMETRY" = 1
perl -MJSON::PP -e 'local $/; open my $f,"<",$ARGV[0] or die; my $c=decode_json(<$f>); my %deny=map {$_=>1} @{$c->{permissions}{deny}}; die unless $deny{Agent} && $deny{WebFetch} && $deny{"mcp__*"}; die unless $c->{permissions}{defaultMode} eq "dontAsk"; die unless $c->{hooks}{PreToolUse}[0]{hooks}[0]{command} eq "/usr/local/bin/ocr-sandbox tool-guard || exit 2"' "$settings"
for call in '"Read","tool_input":{"file_path":"/work/context/PR.md"}' '"Grep","tool_input":{"pattern":"x","path":"/evidence/snapshot"}' '"Glob","tool_input":{"pattern":"*"}' '"Write","tool_input":{"file_path":"/output/report.json","content":"{}"}'; do
  result=$(printf '{"hook_event_name":"PreToolUse","tool_name":%s}' "$call" | /usr/local/bin/ocr-sandbox tool-guard)
  test -z "$result"
done
for call in '"Agent","tool_input":{}' '"WebFetch","tool_input":{}' '"mcp__fixture__read","tool_input":{}' '"Read","tool_input":{"file_path":"/claude-auth/claude/oauth-token"}' '"Bash","tool_input":{"command":"env"}'; do
  status=0
  printf '{"hook_event_name":"PreToolUse","tool_name":%s}' "$call" | /usr/local/bin/ocr-sandbox tool-guard 2>/dev/null || status=$?
  test "$status" = 2
done
read_command=$(perl -MJSON::PP -e 'print encode_json({hook_event_name=>"PreToolUse",tool_name=>"Bash",tool_input=>{command=>"/usr/bin/sed -n \x271,1p\x27 -- \x27/work/context/PR.md\x27",description=>"Read intent"}})')
result=$(printf '%s' "$read_command" | /usr/local/bin/ocr-sandbox tool-guard)
test -z "$result"
cat > /dev/null
test "$(id -u)" = 10001
test "$(id -g)" = 10001
test "$(id -G)" = 10001
perl -ne 'if (/^CapEff:\s+(\w+)/) { exit($1 =~ /^0+$/ ? 0 : 1) }' /proc/self/status
test ! -w /work/context/PR_DIFF.patch
test ! -w /work
test ! -x /ipc
test ! -e /auth/worker-token
test ! -e /output/previous-stage
test "$(cat /work/context/PR_DIFF.patch)" = frozen
test ! -e /work/context/related/support.ts
head=$(perl -MJSON::PP -e 'local $/; open my $f,"<","/evidence/snapshot/manifest.json" or die; print decode_json(<$f>)->{scope}{headSha}')
test "$(cat /evidence/snapshot/trees/$head/support.ts)" = extra
test ! -w /evidence/snapshot/trees/$head/support.ts
if (printf altered > /work/context/PR_DIFF.patch) 2>/dev/null; then exit 90; fi
scenario=$(cat /work/context/PR.md)
printf '{"type":"system","subtype":"init","model":"%s","tools":["Read","Grep","Glob","Bash","Write"]}\n' "$model"
if [ "$scenario" = "Fixture cancel" ]; then sleep 25; else sleep 1; fi
if [ "$scenario" = "Fixture missing-result" ]; then exit 0; fi
if [ "$scenario" = "Fixture invalid-result" ]; then printf '{"type":"result","subtype":"success","is_error":false,"result":"Done","modelUsage":{"%s":{"inputTokens":10,"outputTokens":5}}}\n' "$model"; exit 0; fi
perl -MJSON::PP -e 'local $/; open my $f,"<","/work/context/FROZEN.json" or die; my $id=decode_json(<$f>)->{id}; my $answer=encode_json({report=>{contractVersion=>2,snapshotId=>$id,reviewedFiles=>["a.ts"],missingEvidence=>[],findings=>[]},findingDecisions=>{contractVersion=>2,snapshotId=>$id,decisions=>[]}}); print encode_json({type=>"result",subtype=>"success",is_error=>JSON::PP::false,result=>$answer,modelUsage=>{$ARGV[0]=>{inputTokens=>10,outputTokens=>5}}})."\n"' "$model"
test ! -e /output/report.json
test ! -e /output/finding_decisions.json
printf done > /output/previous-stage
'''


def frozen_packet(model, preflight, scenario):
    encode = lambda value: json.dumps(value, separators=(",", ":"), ensure_ascii=False)
    head, before = "a" * 40, "b" * 40
    entries, blobs, frames, offset = [], [], [], 0
    for path, content in [("a.ts", b"source"), ("support.ts", b"extra")]:
        digest = hashlib.sha256(content).hexdigest()
        source = {"path": path, "revision": head, "mode": "100644", "gitBlobId": hashlib.sha1(b"blob " + str(len(content)).encode() + b"\0" + content).hexdigest(), "bytes": len(content)}
        entries.append({"source": source, "contentHash": digest, "exclusion": None})
        blobs.append({"hash": digest, "offset": offset, "bytes": len(content)})
        frames.append({"operation": "Blob", "hash": digest, "data": base64.b64encode(content).decode()})
        offset += len(content)
    manifest = encode({"version": 1, "policyVersion": 2, "scope": {"host": "github.com", "repository": "fixture/repo", "headSha": head, "baseSha": before, "mergeBaseSha": before, "priorSha": None}, "entries": entries, "blobs": blobs})
    descriptor = {"version": 1, "id": hashlib.sha256(manifest.encode()).hexdigest(), "manifestBytes": len(manifest.encode()), "blobBytes": offset, "entryCount": len(entries), "expandedBytes": offset}
    stage = {"files": ["a.ts"], "before": before, "snapshotId": descriptor["id"], "diffHash": hashlib.sha256(b"frozen").hexdigest()}
    files = {"context/PR_DIFF.patch": "frozen", "context/changed/a.ts": "source", "context/PR.md": "Fixture " + scenario, "context/skill/SKILL.md": "Review", "context/FROZEN.json": encode(descriptor)}
    stage["contextHashes"] = {path: hashlib.sha256(content.encode()).hexdigest() for path, content in files.items()}
    files["context/STAGE.json"] = encode(stage)
    request = {"operation": "Run", "model": model, "version": "0.0.1", "prompt": "Read /work/context/PR_DIFF.patch and /evidence/snapshot; return the structured report in the final response", "files": files, "archive_id": descriptor["id"], "preflight": preflight, "account": 1}
    return {"request": request, "frames": [{"operation": "Install", "descriptor": descriptor, "manifest": manifest, "engine_version": ENGINE_VERSION}, *frames, {"operation": "Seal"}]}


def rpc(client, scenario="intent", control="", catalog=False, preflight=False, model=MODEL):
    packet = {"request": {"operation": "Catalog"}} if catalog else frozen_packet(model, preflight, scenario)
    output = docker("exec", "-i", "-e", "SMOKE_CONTROL=" + control, "-e", "SMOKE_SOCKET=" + sockets[client], client, "perl", "-e", CLIENT, input=json.dumps(packet), timeout=45).stdout
    return [json.loads(line) for line in output.splitlines()]


def main():
    global ENGINE_VERSION
    ENGINE_VERSION = docker("run", "--rm", "--network=none", "--read-only", "--memory=256m", "--cpus=0.5", IMAGE, "engine-version").stdout.strip()
    assert ENGINE_VERSION, "Executor identity unavailable"
    with tempfile.TemporaryDirectory(prefix=PREFIX) as temporary:
        root = pathlib.Path(temporary)
        fake = root / "claude"
        fake.write_text(FAKE)
        fake.chmod(0o755)
        # The sign-in projection in a volume, as the server writes it: the model UID cannot enter
        # app-managed. A volume keeps Linux permissions on every host, where a bind mount may not.
        auth = PREFIX + "-auth"
        docker("volume", "create", auth)
        volumes.append(auth)
        docker("run", "--rm", "--network=none", "--entrypoint=/bin/sh", "--mount", f"type=volume,source={auth},target=/claude-auth", IMAGE, "-c", "mkdir -p /claude-auth/app-managed/accounts/1/claude && printf fixture-not-a-real-credential > /claude-auth/app-managed/accounts/1/claude/oauth-token && chmod 600 /claude-auth/app-managed/accounts/1/claude/oauth-token && chmod 700 /claude-auth/app-managed")
        real_version = docker("run", "--rm", "--network=none", "--read-only", "--user=10001:10001", "--cap-drop=ALL", "--memory=256m", "--cpus=0.5", "--tmpfs", "/tmp:size=16m,mode=1777", "-e", "HOME=/tmp", "--entrypoint=/opt/claude/bin/claude", IMAGE, "--version").stdout.strip()
        expected_version = docker("image", "inspect", "--format", '{{index .Config.Labels "ai.claude.cli.version"}}', IMAGE).stdout.strip()
        assert expected_version and real_version.split()[:1] == [expected_version], "Embedded CLI differs from the pinned image version"
        print("PASS embedded pinned CLI --version as UID 10001 (offline)")
        outbound = PREFIX + "-egress"
        docker("network", "create", outbound)
        networks.append(outbound)
        egress = PREFIX + "-proxy"
        start(egress, ["--network", outbound, "--user=10001:10001", IMAGE, "egress"])
        clients, runners = [], []
        for lane in ["a", "b"]:
            network, volume = PREFIX + "-" + lane, PREFIX + "-ipc-" + lane
            docker("network", "create", "--internal", network)
            networks.append(network)
            docker("volume", "create", volume)
            volumes.append(volume)
            docker("network", "connect", "--alias", "review-egress", network, egress)
            runner, client = PREFIX + "-runner-" + lane, PREFIX + "-client-" + lane
            runners.append(runner)
            clients.append(client)
            sockets[client] = f"/ipc/{runner}.sock"
            start(runner, ["--hostname", runner, "--network", network, "--cap-add=SETUID", "--cap-add=SETGID", "--cap-add=CHOWN", "--cap-add=DAC_OVERRIDE", "--cap-add=KILL", "--mount", f"type=volume,source={volume},target=/ipc", "--mount", f"type=bind,source={fake},target=/opt/claude/bin/claude,readonly", "--mount", f"type=volume,source={auth},target=/claude-auth,readonly", "--tmpfs", "/work:size=16m,mode=0755", "--tmpfs", "/evidence:size=512m,mode=0755", "--tmpfs", "/output:size=48m,mode=0777", "--tmpfs", "/scratch:size=128m,mode=0755", "--tmpfs", "/tmp:size=64m,mode=1777", IMAGE, "runner"])
            start(client, ["--network=none", "--mount", f"type=volume,source={volume},target=/ipc", "--entrypoint=/bin/sleep", IMAGE, "900"])
            for attempt in range(30):
                if docker("exec", runner, "test", "-S", sockets[client], check=False).returncode == 0:
                    break
                time.sleep(0.1)
            else:
                raise RuntimeError("Runner did not expose its private socket: " + docker("logs", runner).stderr)
        for runner in runners:
            limits = json.loads(docker("inspect", "--format", "{{json .HostConfig}}", runner).stdout)
            assert limits["ReadonlyRootfs"] and limits["Memory"] == 512 * 1024 * 1024 and limits["NanoCpus"] == 500_000_000 and limits["PidsLimit"] == 64
            actual = docker("exec", runner, "cat", "/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/pids.max", "/sys/fs/cgroup/cpu.max").stdout.splitlines()
            assert actual[:2] == [str(512 * 1024 * 1024), "64"], actual
            quota, period = map(int, actual[2].split())
            assert quota / period == 0.5, actual
        print("PASS cgroup CPU/memory/PID limits and read-only rootfs")
        catalog = rpc(clients[0], catalog=True)
        assert catalog[-1]["event"] == "Catalog", catalog
        assert MODEL in catalog[-1]["models"], catalog
        print("PASS private socket and discovered review models")
        prepared = rpc(clients[0], preflight=True)
        assert prepared[-1]["event"] == "Ready" and not any(event["event"] == "Started" for event in prepared), prepared
        print("PASS archive installation and exact CLI preflight without inference")
        rejected = rpc(clients[0], model="unavailable-model")
        assert rejected[-1]["event"] == "Error" and "is not a model" in rejected[-1]["error"], rejected
        assert not any(event["event"] == "Started" for event in rejected), rejected
        print("PASS unknown models prevent inference")
        for scenario in ["missing-result", "invalid-result"]:
            failed = rpc(clients[0], scenario)
            assert failed[-1]["event"] == "Finished" and "final response" in failed[-1]["error"], failed
            assert failed[-1]["report"] is None and failed[-1]["decisions"] is None, failed
        result = rpc(clients[0])[-1]
        assert result["error"] is None and json.loads(result["report"])["reviewedFiles"] == ["a.ts"], result
        assert json.loads(result["decisions"])["decisions"] == [], result
        print("PASS final response supplies report and decisions without model-created files; missing/invalid answers fail")
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            cancelled = pool.submit(rpc, clients[0], "cancel", "cancel")
            sibling = pool.submit(rpc, clients[1])
            a, b = cancelled.result(), sibling.result()
        assert a[-1]["event"] == "Finished" and "cancel" in a[-1]["error"].lower(), a
        assert a[-1]["transcript"] and '"subtype":"init"' in a[-1]["transcript"], a
        assert b[-1]["event"] == "Finished" and b[-1]["error"] is None, b
        starts = [next(event["receivedAt"] for event in events if event["event"] == "Started") for events in [a, b]]
        assert max(starts) < min(a[-1]["receivedAt"], b[-1]["receivedAt"]), "No actual process overlap"
        print("PASS two simultaneous runners; independent cancellation and partial transcript")
        disconnected = rpc(clients[0], "cancel", "disconnect")
        assert disconnected[-1]["event"] == "Started"
        time.sleep(0.8)
        recovered = rpc(clients[0])
        assert recovered[-1]["event"] == "Finished" and recovered[-1]["error"] is None, recovered
        print("PASS disconnect kills inference; next stage has clean scratch")
        exposed = json.dumps([rpc(clients[0]), docker("logs", runners[0]).stdout, docker("logs", runners[0]).stderr])
        assert "fixture-not-a-real-credential" not in exposed
        print("PASS seeded credential never appears in IPC results or runner logs")
        for runner in runners:
            denied = docker("exec", "--user=10001:10001", runner, "perl", "-MIO::Socket::UNIX", "-e", "exit(IO::Socket::UNIX->new(Type=>SOCK_STREAM,Peer=>'/ipc/" + runner + ".sock') ? 1 : 0)", check=False)
            assert denied.returncode == 0, "Model UID reached supervisor socket"
        production = docker("inspect", "--format", "{{json .NetworkSettings.Networks}}", "opencodereview-server-1", check=False)
        targets = ["http://host.docker.internal:8787/health"]
        gateway = docker("network", "inspect", "--format", "{{(index .IPAM.Config 0).Gateway}}", PREFIX + "-a").stdout.strip()
        if gateway:
            targets.append("http://" + gateway + ":8787/health")
        if production.returncode == 0:
            targets.extend("http://" + network["IPAddress"] + ":8787/health" for network in json.loads(production.stdout).values() if network.get("IPAddress"))
        for target in targets:
            result = docker("exec", "--user=10001:10001", runners[0], "curl", "--noproxy", "*", "--max-time", "2", "--connect-timeout", "1", "-sS", "-o", "/dev/null", "-w", "%{http_code}", target, check=False)
            assert result.returncode != 0 and result.stdout == "000", "Runner can reach operational host/container"
        reachable = docker("exec", "--user=10001:10001", runners[0], "perl", "-MIO::Socket::INET", "-e", "exit(IO::Socket::INET->new(PeerAddr=>'review-egress',PeerPort=>8790,Timeout=>2) ? 0 : 1)", check=False)
        assert reachable.returncode == 0, "Restricted egress proxy is not reachable"
        blocked = docker("exec", "--user=10001:10001", runners[0], "curl", "--max-time", "3", "--proxy", "http://review-egress:8790", "-sS", "-o", "/dev/null", "https://example.com", check=False)
        assert blocked.returncode != 0, "Proxy accepted an unapproved provider"
        print("PASS model UID cannot reach IPC/production; proxy rejects unapproved host")
        docker("restart", runners[0])
        time.sleep(0.5)
        restarted = rpc(clients[0])
        assert restarted[-1]["event"] == "Finished" and restarted[-1]["error"] is None, restarted
        print("PASS stale socket recovery after runner restart; no automatic work replay")
        print("PASS Docker smoke completed; no real inference or account login was used")


if __name__ == "__main__":
    try:
        main()
    finally:
        for name in reversed(containers):
            docker("stop", "--time", "2", name, check=False, timeout=10)
            docker("rm", "-v", name, check=False)
        for name in reversed(networks):
            docker("network", "rm", name, check=False)
        for name in reversed(volumes):
            docker("volume", "rm", name, check=False)
