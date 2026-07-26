#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="${PREFIX:-/usr/local}"
DESTDIR="${DESTDIR:-}"

cargo build --locked --release --manifest-path "${ROOT}/Cargo.toml"
install -Dm755 "${ROOT}/target/release/rs-nomadnet" \
    "${DESTDIR}${PREFIX}/bin/rs-nomadnet"
install -Dm644 "${ROOT}/contrib/rsnomadnet.service" \
    "${DESTDIR}/usr/lib/systemd/system/rsnomadnet.service"

echo "Installed rs-nomadnet under ${DESTDIR}${PREFIX}"
echo "Review /etc/reticulum, then enable rsnomadnet.service if desired."
