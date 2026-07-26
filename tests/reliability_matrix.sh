#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo test --manifest-path "${ROOT}/../rsReticulum/Cargo.toml" \
    -p rns-link test_stale_recovery
cargo test --manifest-path "${ROOT}/../rsReticulum/Cargo.toml" \
    -p rns-protocol test_check_timeout_triggers_retry
cargo test --manifest-path "${ROOT}/../rsReticulum/Cargo.toml" \
    -p rns-protocol test_inbound_duplicate_part_is_noop
cargo test --manifest-path "${ROOT}/../rsReticulum/Cargo.toml" \
    -p rns-protocol test_proof_validation
cargo test --manifest-path "${ROOT}/../rsLXMF/Cargo.toml" \
    -p lxmf-core corrupt_ring_is_preserved_and_never_advertised
cargo test --manifest-path "${ROOT}/../rsLXMF/Cargo.toml" \
    -p lxmf-core test_reject_duplicate
cargo test --manifest-path "${ROOT}/../rsLXMF/Cargo.toml" \
    -p lxmf-core test_propagation_node_announce_trigger_clears_propagated_retry_backoff
cargo test --manifest-path "${ROOT}/Cargo.toml" \
    db::tests::outbound_queue_survives_state_transitions
cargo test --manifest-path "${ROOT}/Cargo.toml" \
    db::tests::migrates_every_previously_released_schema

echo "Reliability fault matrix passed"
