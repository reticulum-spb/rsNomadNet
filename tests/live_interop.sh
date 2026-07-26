#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RNS_CONFIG="${1:-${HOME}/.rsReticulum}"
MODE="${2:-all}"
PYTHON="${ROOT}/.venv/bin/python"
ARTIFACTS="$(mktemp -d /tmp/rsnomadnet-live.XXXXXX)"
HUB_PID=""
NOMAD_PID=""
LXMD_PID=""

stop_process() {
    local pid="${1:-}"
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
        kill -INT "${pid}" 2>/dev/null || true
        wait "${pid}" 2>/dev/null || true
    fi
}

cleanup() {
    stop_process "${LXMD_PID}"
    stop_process "${NOMAD_PID}"
    stop_process "${HUB_PID}"
}
trap cleanup EXIT

if [[ ! -x "${PYTHON}" ]]; then
    echo "Missing ${PYTHON}; create it with:" >&2
    echo "  python3.11 -m venv .venv" >&2
    echo "  .venv/bin/pip install -e ../Reticulum -e ../LXMF -e ../NomadNet" >&2
    exit 2
fi
"${PYTHON}" -c 'import RNS, LXMF, nomadnet'
command -v curl >/dev/null
command -v jq >/dev/null
test -d "${RNS_CONFIG}"

if [[ "${MODE}" == "preflight" ]]; then
    echo "Live interop preflight passed"
    exit 0
fi

cargo build --manifest-path "${ROOT}/Cargo.toml"
cargo build --manifest-path "${ROOT}/../rsRRCD/Cargo.toml" --bin rrcd-rs
cargo build --manifest-path "${ROOT}/../rsLXMF/Cargo.toml" --bin lxmd-rs

if [[ "${MODE}" == "all" || "${MODE}" == "rrc" ]]; then
    RSRRCD_HOME="${ARTIFACTS}/rrcd" \
        "${ROOT}/../rsRRCD/target/debug/rrcd-rs" >"${ARTIFACTS}/rrcd-init.log" 2>&1 || true
    RSRRCD_HOME="${ARTIFACTS}/rrcd" \
        "${ROOT}/../rsRRCD/target/debug/rrcd-rs" >"${ARTIFACTS}/rrcd.log" 2>&1 &
    HUB_PID=$!
    for _ in $(seq 1 300); do
        HUB_DEST="$(sed -n 's/.*destination <\([0-9a-f]*\)>.*/\1/p' "${ARTIFACTS}/rrcd.log" | head -n1)"
        [[ -n "${HUB_DEST}" ]] && break
        sleep 0.1
    done
    test -n "${HUB_DEST:-}"
    "${PYTHON}" "${ROOT}/tests/python_rrc_smoke.py" \
        "${HUB_DEST}" "${RNS_CONFIG}" "${ARTIFACTS}/python-rrc" "interop"
    stop_process "${HUB_PID}"
    HUB_PID=""
fi

if [[ "${MODE}" == "all" || "${MODE}" == "lxmf" ]]; then
    "${ROOT}/target/debug/rs-nomadnet" \
        --listen 127.0.0.1:18080 \
        --state-dir "${ARTIFACTS}/nomad" \
        --rns-config "${RNS_CONFIG}" >"${ARTIFACTS}/nomad.log" 2>&1 &
    NOMAD_PID=$!
    for _ in $(seq 1 300); do
        STATE="$(curl -fsS http://127.0.0.1:18080/api/v1/state 2>/dev/null || true)"
        NOMAD_DEST="$(printf '%s' "${STATE}" | sed -n 's/.*"destination_hash":"\([0-9a-f]*\)".*/\1/p')"
        [[ -n "${NOMAD_DEST}" ]] && break
        sleep 0.1
    done
    test -n "${NOMAD_DEST:-}"

    RUST_MARKER="lxmd-rs-interop-$(date +%s)"
    "${ROOT}/../rsLXMF/target/debug/lxmd-rs" \
        --config "${ARTIFACTS}/lxmd-client" \
        --rnsconfig "${RNS_CONFIG}" \
        --send "${NOMAD_DEST}" "${RUST_MARKER}" \
        --send-method opportunistic
    PYTHON_MARKER="python-lxmf-interop-$(date +%s)"
    "${PYTHON}" "${ROOT}/tests/python_lxmf_send.py" \
        "${NOMAD_DEST}" "${RNS_CONFIG}" "${ARTIFACTS}/python-lxmf" "${PYTHON_MARKER}"

    for marker in "${RUST_MARKER}" "${PYTHON_MARKER}"; do
        for _ in $(seq 1 600); do
            curl -fsS http://127.0.0.1:18080/api/v1/conversations |
                grep -q "${marker}" && break
            sleep 0.1
        done
        curl -fsS http://127.0.0.1:18080/api/v1/conversations |
            grep -q "${marker}"
    done

    BEFORE_PROPAGATION_COUNT="$(
        curl -fsS http://127.0.0.1:18080/api/v1/directory |
            jq '[.[] | select(.kind == "propagation")] | length'
    )"
    mkdir -p "${ARTIFACTS}/lxmd-node"
    install -m 600 "${ROOT}/tests/lxmd_interop_config" \
        "${ARTIFACTS}/lxmd-node/config"
    "${ROOT}/../rsLXMF/target/debug/lxmd-rs" \
        --config "${ARTIFACTS}/lxmd-node" \
        --rnsconfig "${RNS_CONFIG}" \
        --propagation-node --service -v >"${ARTIFACTS}/lxmd.log" 2>&1 &
    LXMD_PID=$!
    for _ in $(seq 1 600); do
        grep -q "Startup propagation announce sent" "${ARTIFACTS}/lxmd.log" &&
            AFTER_PROPAGATION_COUNT="$(
                curl -fsS http://127.0.0.1:18080/api/v1/directory |
                    jq '[.[] | select(.kind == "propagation")] | length'
            )" &&
            ((AFTER_PROPAGATION_COUNT > BEFORE_PROPAGATION_COUNT)) &&
            break
        sleep 0.1
    done
    test "${AFTER_PROPAGATION_COUNT:-0}" -gt "${BEFORE_PROPAGATION_COUNT}"

    if [[ -n "${PYTHON_NOMADNET_DESTINATION:-}" ]]; then
        curl -fsS -X POST http://127.0.0.1:18080/api/v1/browser/fetch \
            -H 'content-type: application/json' \
            -d "{\"url\":\"${PYTHON_NOMADNET_DESTINATION}:/page/index.mu\"}" |
            grep -q '"blocks"'
    elif [[ "${REQUIRE_PYTHON_NOMADNET:-0}" == "1" ]]; then
        echo "PYTHON_NOMADNET_DESTINATION is required" >&2
        exit 2
    fi
fi

echo "Live interoperability passed; artifacts: ${ARTIFACTS}"
