#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${1:-18082}"
BASE="http://127.0.0.1:${PORT}"
TOKEN="rsnomadnet-security-smoke-token-0123456789abcdef"
ARTIFACTS="$(mktemp -d /tmp/rsnomadnet-security.XXXXXX)"
SERVER_PID=""

cleanup() {
    if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
        kill -INT "${SERVER_PID}" 2>/dev/null || true
        wait "${SERVER_PID}" 2>/dev/null || true
    fi
    if [[ "${ARTIFACTS}" == /tmp/rsnomadnet-security.* ]]; then
        rm -rf -- "${ARTIFACTS}"
    fi
}
trap cleanup EXIT

install -m 600 /dev/null "${ARTIFACTS}/token"
printf '%s\n' "${TOKEN}" >>"${ARTIFACTS}/token"
truncate -s 2097153 "${ARTIFACTS}/oversized"

cargo build --manifest-path "${ROOT}/Cargo.toml"
"${ROOT}/target/debug/rs-nomadnet" \
    --offline \
    --listen "127.0.0.1:${PORT}" \
    --state-dir "${ARTIFACTS}/state" \
    --auth-token-file "${ARTIFACTS}/token" >"${ARTIFACTS}/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 100); do
    curl -fsS "${BASE}/api/v1/health" >/dev/null 2>&1 && break
    sleep 0.1
done
curl -fsS "${BASE}/api/v1/health" >/dev/null

test "$(curl -sS -o /dev/null -w '%{http_code}' "${BASE}/api/v1/state")" = "401"
curl -fsS -H "authorization: Bearer ${TOKEN}" \
    "${BASE}/api/v1/state" >/dev/null
test "$(
    curl -sS -o /dev/null -w '%{http_code}' \
        -X POST \
        -H "authorization: Bearer ${TOKEN}" \
        -H "origin: https://attacker.example" \
        "${BASE}/api/v1/conversations/00000000000000000000000000000000/read"
)" = "403"
test "$(
    curl -sS -o /dev/null -w '%{http_code}' \
        -X POST \
        -H "authorization: Bearer ${TOKEN}" \
        -H "content-type: application/json" \
        --data-binary "@${ARTIFACTS}/oversized" \
        "${BASE}/api/v1/messages"
)" = "413"
curl -fsSI "${BASE}/" | grep -qi '^content-security-policy:'

echo "Security smoke test passed"
