"""Checks the newest release of each AI CLI before it is pinned in docker/Dockerfile.

  resolve          Print the Docker build arguments for the newest Claude Code, Codex and Grok
                   releases (linux/amd64) and write a version table to the step summary.
  flags IMAGE      Check that each CLI in IMAGE still accepts every flag its provider passes.

No step reads credentials or calls a model.
"""

import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
CLAUDE_RELEASES = "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases"
GROK_ARTIFACTS = "https://storage.googleapis.com/grok-build-public-artifacts/cli"


def fetch(url, binary=False):
    request = urllib.request.Request(url, headers={"User-Agent": "opencodereview-provider-cli-canary"})
    if "api.github.com" in url and os.environ.get("GH_TOKEN"):
        request.add_header("Authorization", "Bearer " + os.environ["GH_TOKEN"])
    with urllib.request.urlopen(request, timeout=120) as response:
        body = response.read()
    return body if binary else body.decode()


def sha256(url):
    return hashlib.sha256(fetch(url, binary=True)).hexdigest()


def latest_github(repository, prefix):
    tag = json.loads(fetch(f"https://api.github.com/repos/{repository}/releases/latest"))["tag_name"]
    return tag.removeprefix(prefix)


def pinned():
    dockerfile = (ROOT / "docker/Dockerfile").read_text()
    return dict(re.findall(r"^ARG (\w+_CLI_VERSION)=(\S+)$", dockerfile, re.M))


def resolve():
    claude = fetch(f"{CLAUDE_RELEASES}/latest").strip()
    codex = latest_github("openai/codex", "rust-v")
    grok = fetch(f"{GROK_ARTIFACTS}/stable").strip().splitlines()[0].strip()
    manifest = json.loads(fetch(f"{CLAUDE_RELEASES}/{claude}/manifest.json"))
    args = {
        "CLAUDE_CLI_VERSION": claude,
        "CLAUDE_CLI_SHA256_AMD64": manifest["platforms"]["linux-x64"]["checksum"],
        "CODEX_CLI_VERSION": codex,
        "CODEX_CLI_SHA256_AMD64": sha256(f"https://github.com/openai/codex/releases/download/rust-v{codex}/codex-x86_64-unknown-linux-musl.tar.gz"),
        "CODEX_CODE_MODE_HOST_SHA256_AMD64": sha256(f"https://github.com/openai/codex/releases/download/rust-v{codex}/codex-code-mode-host-x86_64-unknown-linux-musl.tar.gz"),
        "GROK_CLI_VERSION": grok,
        "GROK_CLI_SHA256_AMD64": sha256(f"{GROK_ARTIFACTS}/grok-{grok}-linux-x86_64"),
    }
    current = pinned()
    rows = ["| CLI | Pinned | Newest |", "| --- | --- | --- |"]
    for name in ["CLAUDE", "CODEX", "GROK"]:
        key = name + "_CLI_VERSION"
        mark = "" if current.get(key) == args[key] else " (new)"
        rows.append(f"| {name.title()} | {current.get(key, '?')} | {args[key]}{mark} |")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as file:
            file.write("## Provider CLI releases\n\n" + "\n".join(rows) + "\n")
    print("\n".join(rows), file=sys.stderr)
    for key, value in args.items():
        print(f"{key}={value}")


def provider_flags(name):
    source = (ROOT / f"backend/src/providers/{name}.rs").read_text().split("#[cfg(test)]")[0]
    return sorted(set(re.findall(r'"(--[a-z][a-z-]*)"', source)))


def run(image, *command, entrypoint=None, env=(), stdin=None, timeout=60):
    args = ["docker", "run", "--rm", "-i", "-e", "HOME=/tmp/home", *[part for pair in env for part in ("-e", pair)]]
    if entrypoint:
        args += ["--entrypoint", entrypoint]
    result = subprocess.run([*args, image, *command], input=stdin, text=True, capture_output=True, timeout=timeout)
    return result.returncode, result.stdout + result.stderr


def flags(image):
    # The help screen of the subcommand each provider runs.
    help_commands = {
        "claude": ["claude", "--help"],
        "codex": ["codex", "exec", "--help"],
        "grok": ["grok", "--help"],
    }
    failures = []
    for name, command in help_commands.items():
        _, output = run(image, "sh", "-c", 'mkdir -p "$HOME" && exec "$@"', "sh", *command, entrypoint="")
        expected = provider_flags(name)
        missing = [flag for flag in expected if not re.search(re.escape(flag) + r"(?![a-z-])", output)]
        print(f"{'FAIL' if missing else 'PASS'} {name}: {len(expected) - len(missing)}/{len(expected)} flags in {' '.join(command)}")
        if missing:
            failures.append(f"{name} no longer lists {', '.join(missing)}")
    if failures:
        sys.exit("\n".join(failures))


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command == "resolve":
        resolve()
    elif command == "flags" and len(sys.argv) == 3:
        globals()[command](sys.argv[2])
    else:
        sys.exit(__doc__)
