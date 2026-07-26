#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <state-directory> <backup.tar.gz>" >&2
    exit 2
fi

STATE_DIR="$(realpath "$1")"
OUTPUT="$(realpath -m "$2")"
DATABASE="${STATE_DIR}/nomadnet.db"
IDENTITY="${STATE_DIR}/identity"
BACKUP_TMP="$(mktemp -d /tmp/rsnomadnet-backup.XXXXXX)"

cleanup() {
    if [[ "${BACKUP_TMP}" == /tmp/rsnomadnet-backup.* ]]; then
        rm -rf -- "${BACKUP_TMP}"
    fi
}
trap cleanup EXIT

test -f "${DATABASE}"
command -v sqlite3 >/dev/null
sqlite3 "${DATABASE}" ".backup '${BACKUP_TMP}/nomadnet.db'"
if [[ -f "${IDENTITY}" ]]; then
    install -m 600 "${IDENTITY}" "${BACKUP_TMP}/identity"
fi
install -m 600 /dev/null "${BACKUP_TMP}/MANIFEST"
{
    echo "format=rsnomadnet-backup-v1"
    echo "created_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >>"${BACKUP_TMP}/MANIFEST"
tar -C "${BACKUP_TMP}" -czf "${OUTPUT}" .
chmod 600 "${OUTPUT}"
echo "Backup written to ${OUTPUT}"
