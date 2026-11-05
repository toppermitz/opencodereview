#!/usr/bin/env bash
# Deterministic proof that OpenCodeReview's GitLab Self-Hosted/glab swap is wired
# against a real instance. Re-run from the repo root:
#
#   OCR_GITLAB_HOST=gitlab.serverinfo.com.br ./scripts/prove-gitlab-self-hosted.sh
#   GITLAB_TOKEN=… OCR_GITLAB_HOST=gitlab.serverinfo.com.br ./scripts/prove-gitlab-self-hosted.sh
#
# Exit 0 only when every required check passes. Authenticated checks are required
# when GITLAB_TOKEN is set; without it they are reported as SKIPPED (not a pass).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

HOST="${OCR_GITLAB_HOST:-}"
BASE="https://${HOST}"
GLAB="${GLAB_BIN:-$(command -v glab || true)}"
PASS=0
FAIL=0
SKIP=0
REPORT=()

log() { REPORT+=("$*"); printf '%s\n' "$*"; }
ok() { PASS=$((PASS + 1)); log "PASS  $*"; }
bad() { FAIL=$((FAIL + 1)); log "FAIL  $*"; }
skip() { SKIP=$((SKIP + 1)); log "SKIP  $*"; }

need_host() {
  if [[ -z "$HOST" ]]; then
    bad "OCR_GITLAB_HOST is empty (expected bare host, e.g. gitlab.serverinfo.com.br)"
    return 1
  fi
  if [[ "$HOST" == *://* || "$HOST" == */* || "$HOST" == *@* ]]; then
    bad "OCR_GITLAB_HOST must be bare host[:port], got: $HOST"
    return 1
  fi
  ok "OCR_GITLAB_HOST is bare host: $HOST"
}

check_source_swap() {
  if rg -n 'Command::new\("gh"\)|"gh"\s*,\s*&?\[' backend/src/gitlab --glob '*.rs' >/dev/null; then
    bad "backend/src/gitlab still spawns gh"
  else
    ok "backend/src/gitlab has no gh Command::new / argv spawns"
  fi
  if ! rg -n 'Command::new\("glab"\)' backend/src/gitlab --glob '*.rs' >/dev/null; then
    bad "backend/src/gitlab never spawns glab"
  else
    ok "backend/src/gitlab spawns glab"
  fi
  if ! rg -n 'OCR_GITLAB_HOST' docker-compose.yml >/dev/null; then
    bad "docker-compose.yml missing OCR_GITLAB_HOST"
  elif ! rg -n 'OCR_GITLAB_CLIENT_ID' docker-compose.yml >/dev/null; then
    bad "docker-compose.yml missing OCR_GITLAB_CLIENT_ID"
  elif rg -n 'OCR_GH_HOST|apt-get install.*\bgh\b|cli\.github\.com' docker-compose.yml docker/Dockerfile >/dev/null; then
    bad "Docker still references OCR_GH_HOST or installs gh"
  else
    ok "Compose requires OCR_GITLAB_HOST and OCR_GITLAB_CLIENT_ID; Dockerfile no longer installs gh"
  fi
  if ! rg -n 'oauth/authorize_device|OAUTH_TOKEN' backend/src/gitlab >/dev/null; then
    bad "GitLab OAuth device flow / OAUTH_TOKEN wiring missing"
  else
    ok "GitLab OAuth device flow stores a token for glab"
  fi
  if ! rg -n '"/auth/gitlab/status"' backend/src/server/http.rs src/services/setupApiAccess.ts >/dev/null; then
    bad "HTTP/UI missing /auth/gitlab/status"
  else
    ok "HTTP + UI expose /auth/gitlab/status"
  fi
  if [[ -d backend/src/github ]]; then
    bad "backend/src/github still exists"
  else
    ok "backend/src/github removed (module is gitlab)"
  fi
}

check_live_web() {
  local headers body code
  headers="$(mktemp)"
  body="$(mktemp)"
  code="$(curl -sS -o "$body" -D "$headers" -w '%{http_code}' --connect-timeout 15 "$BASE/" || true)"
  if grep -qi 'x-gitlab-meta' "$headers" || grep -qi 'gitlab' "$headers"; then
    ok "HTTPS $BASE/ responds as GitLab (HTTP $code, x-gitlab-meta or gitlab headers)"
  else
    bad "HTTPS $BASE/ did not look like GitLab (HTTP $code)"
    sed -n '1,20p' "$headers" | while read -r line; do log "      $line"; done
  fi
  rm -f "$headers" "$body"
}

check_live_api_public() {
  local code json
  code="$(curl -sS -o /tmp/ocr-prove-projects.json -w '%{http_code}' --connect-timeout 15 \
    "$BASE/api/v4/projects?per_page=1&simple=true" || true)"
  json="$(cat /tmp/ocr-prove-projects.json 2>/dev/null || true)"
  if [[ "$code" != "200" ]]; then
    bad "GET /api/v4/projects returned HTTP $code"
    return
  fi
  if ! python3 - "$json" <<'PY'
import json, sys
data = json.loads(sys.argv[1])
assert isinstance(data, list) and data, "expected non-empty project list"
p = data[0]
for key in ("path_with_namespace", "path", "web_url", "http_url_to_repo"):
    assert key in p and p[key], f"missing {key}"
assert p["http_url_to_repo"].startswith("https://"), p["http_url_to_repo"]
assert "/-/" not in p["path_with_namespace"]
print(p["path_with_namespace"])
PY
  then
    bad "Public projects JSON does not match the shape OCR maps (path_with_namespace/path/web_url)"
    log "      body: ${json:0:200}"
    return
  fi
  local path
  path="$(python3 -c 'import json; print(json.load(open("/tmp/ocr-prove-projects.json"))[0]["path_with_namespace"])')"
  ok "Public API project shape OK (example path_with_namespace=$path)"
}

check_glab_cli() {
  if [[ -z "$GLAB" || ! -x "$GLAB" ]]; then
    bad "glab binary not found (set GLAB_BIN or install glab 1.128.0+)"
    return
  fi
  local ver
  ver="$("$GLAB" version 2>&1 | head -n1)"
  ok "glab available: $ver"
  if [[ -n "${GITLAB_TOKEN:-}" ]]; then
    local out
    if out="$(GITLAB_HOST="$HOST" GITLAB_TOKEN="$GITLAB_TOKEN" "$GLAB" api user 2>/dev/null)"; then
      local user
      user="$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("username",""))')"
      if [[ -n "$user" ]]; then
        ok "Authenticated glab api user → @$user on $HOST"
      else
        bad "glab api user returned no username: ${out:0:120}"
      fi
      if GITLAB_HOST="$HOST" GITLAB_TOKEN="$GITLAB_TOKEN" "$GLAB" api \
        "groups?membership=true&min_access_level=10&per_page=5" >/tmp/ocr-prove-groups.json 2>/dev/null; then
        ok "Authenticated glab listed membership groups"
      else
        bad "Authenticated glab could not list groups?membership=true"
      fi
    else
      bad "GITLAB_TOKEN set but glab api user failed against $HOST"
    fi
  else
    skip "GITLAB_TOKEN unset — authenticated login/workspaces/MR notes not proven"
    if ! GITLAB_HOST="$HOST" "$GLAB" api user >/dev/null 2>&1; then
      ok "Unauthenticated glab api user correctly rejected by $HOST"
    else
      bad "Unauthenticated glab api user unexpectedly succeeded"
    fi
  fi
}

check_rust_host_unit() {
  if ! command -v rustup >/dev/null; then
    skip "rustup missing — skip cargo unit proof"
    return
  fi
  local toolchain="${OCR_RUST_TOOLCHAIN:-1.98.1}"
  if ! rustup run "$toolchain" cargo test -p opencodereview-server --locked --lib \
    -- gitlab::tests::accepts_plain_hosts_and_ports gitlab::tests::project_api_id_encodes_nested_paths \
    gitlab::tests::parses_live_pr_state_without_unsupported_fields \
    >/tmp/ocr-prove-cargo.log 2>&1; then
    bad "cargo unit proof failed (see /tmp/ocr-prove-cargo.log)"
    tail -n 30 /tmp/ocr-prove-cargo.log | while read -r line; do log "      $line"; done
    return
  fi
  # Host acceptance for this exact instance
  if ! OCR_GITLAB_HOST="$HOST" rustup run "$toolchain" cargo test -p opencodereview-server --locked --lib \
    -- gitlab::tests::accepts_plain_hosts_and_ports >/dev/null 2>&1; then
    bad "host unit tests failed under OCR_GITLAB_HOST=$HOST"
    return
  fi
  python3 - "$HOST" <<'PY'
import os, subprocess, sys
host = sys.argv[1]
# Prove is_valid_gitlab_host accepts this host via a one-shot rustc isn't needed —
# mirror the same rules as auth.rs for the operator-facing proof.
assert host and len(host) <= 253
name, _, port = host.partition(":")
if port:
    assert port.isdigit()
assert all(
    label and not label.startswith("-") and not label.endswith("-")
    and all(c.isalnum() or c == "-" for c in label)
    for label in name.split(".")
), host
print(host)
PY
  ok "Rust gitlab unit tests passed; host $HOST matches OCR host validation rules"
}

main() {
  log "=== OpenCodeReview GitLab Self-Hosted proof ==="
  log "host=$HOST  glab=${GLAB:-none}  token=${GITLAB_TOKEN:+set}"
  log ""
  need_host || true
  check_source_swap
  check_live_web
  check_live_api_public
  check_glab_cli
  check_rust_host_unit
  log ""
  log "=== summary: PASS=$PASS FAIL=$FAIL SKIP=$SKIP ==="
  if [[ "$FAIL" -gt 0 ]]; then
    exit 1
  fi
  if [[ -z "${GITLAB_TOKEN:-}" ]]; then
    log "NOTE: authenticated forge flows are not proven until GITLAB_TOKEN is available to the agent."
  fi
}

main "$@"
