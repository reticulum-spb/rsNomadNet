#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo test --locked --manifest-path "${ROOT}/Cargo.toml"
cargo clippy --locked --manifest-path "${ROOT}/Cargo.toml" \
    --all-targets -- -D warnings
node --test "${ROOT}/web/rrc-ui.test.js"
"${ROOT}/tests/security_smoke.sh"
cargo build --locked --release --manifest-path "${ROOT}/Cargo.toml"

echo "Release verification passed"
