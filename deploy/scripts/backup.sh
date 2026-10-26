#!/usr/bin/env bash
# Backs up the running pacod-gw compose stack into a dated directory:
#
#   <backup root>/<UTC time stamp>/
#       postgres.dump     pg_dump, custom format: configuration, encrypted provider
#                         credentials, audit log, limit counters
#       pacod-id.dump     pg_dump of pacod-id's database "pacod_id" (accounts, roles,
#                         sessions, its signing keys sealed under PID_KEK), when there is one
#       clickhouse.zip    ClickHouse BACKUP of the analytics database: usage events,
#                         audit-mode records, rollups
#       manifest.txt      what was backed up, and the fingerprint of the key-encryption key
#       SHA256SUMS        checksums of the files above
#
# NOT in the backup, on purpose:
#   - the key-encryption keys (PGW_KEK, and pacod-id's PID_KEK) and everything else in
#     deploy/.env. The provider
#     credentials in postgres.dump are encrypted under that key. Whoever has only the backup
#     cannot read them; whoever restores the backup without the key cannot either. Keep a
#     copy of the key somewhere that is neither this host nor the place the backups go.
#   - the event spool (transient), the Caddy certificates (issued again on demand) and the
#     Prometheus and Grafana volumes (metrics history; dashboards come from the repository).
#
# The script does not stop anything. Both dumps are consistent snapshots.
#
# Usage: deploy/scripts/backup.sh [options]
#   --kek-copy FILE    Verify the separately stored copy of PGW_KEK: FILE holds the key and
#                      nothing else. The backup runs only if it matches the key in use.
#   --kek-stored       State that a copy of PGW_KEK is stored separately, without verifying
#                      it. For scheduled runs. PGW_KEK_STORED=yes does the same.
#   --output DIR       Backup root. Default: $PGW_BACKUP_DIR, or deploy/backups.
#   --keep N           Afterwards, delete all but the newest N backups in the backup root.
#   --skip-clickhouse  Back up PostgreSQL only (the minimal setup has no ClickHouse).
#   -h, --help         Show this text.
#
# Without --kek-copy or --kek-stored the script asks, and refuses when it cannot ask.

set -euo pipefail

# shellcheck source=deploy/scripts/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

backup_root="${PGW_BACKUP_DIR:-$DEPLOY_DIR/backups}"
kek_copy=""
kek_stored="${PGW_KEK_STORED:-no}"
keep=""
with_clickhouse=yes

while [ $# -gt 0 ]; do
    case "$1" in
        --kek-copy)
            [ $# -ge 2 ] || die "--kek-copy needs a file"
            kek_copy="$2"
            shift 2
            ;;
        --kek-stored)
            kek_stored=yes
            shift
            ;;
        --output)
            [ $# -ge 2 ] || die "--output needs a directory"
            backup_root="$2"
            shift 2
            ;;
        --keep)
            [ $# -ge 2 ] || die "--keep needs a number"
            keep="$2"
            shift 2
            ;;
        --skip-clickhouse)
            with_clickhouse=no
            shift
            ;;
        -h | --help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done

case "$keep" in
    '' | *[!0-9]*) [ -z "$keep" ] || die "--keep needs a number" ;;
esac

require_tools
umask 077

service_running pacod-postgres || die "the pacod-postgres service is not running. Start the stack first."
require_pg_superuser
if [ "$with_clickhouse" = yes ] && ! service_running gw-clickhouse; then
    die "the clickhouse service is not running. Start it, or pass --skip-clickhouse."
fi

# ---------------------------------------------------------------------------------------------
# The key-encryption key is not in the backup. Make sure it is kept somewhere else.
# ---------------------------------------------------------------------------------------------
keys="$(kek_fingerprints running)" || die "cannot read PGW_KEK from the core container or from the configuration"
current_fingerprint="$(printf '%s\n' "$keys" | awk '$1 == "current" { print $3 }')"
current_id="$(printf '%s\n' "$keys" | awk '$1 == "current" { print $2 }')"

info "The key-encryption key (PGW_KEK, id \"$current_id\", fingerprint $current_fingerprint) is NOT part of"
info "this backup. Without it the provider credentials in the backup cannot be decrypted."

if [ -n "$kek_copy" ]; then
    [ -r "$kek_copy" ] || die "cannot read $kek_copy"
    mkdir -p "$backup_root"
    copy_dir="$(cd "$(dirname "$kek_copy")" && pwd -P)"
    root_dir="$(cd "$backup_root" && pwd -P)"
    case "$copy_dir/" in
        "$root_dir"/*) die "$kek_copy is inside the backup directory. The key must be stored somewhere else." ;;
    esac
    copy_fingerprint="$(fingerprint "$(tr -d ' \t\r\n' <"$kek_copy")")"
    [ "$copy_fingerprint" = "$current_fingerprint" ] ||
        die "$kek_copy does not hold the key in use (its fingerprint is $copy_fingerprint)"
    info "Verified: $kek_copy holds the key in use."
elif [ "$kek_stored" = yes ]; then
    info "Taken as stated: a copy of the key is stored separately."
elif [ -t 0 ]; then
    printf 'Is a copy of PGW_KEK stored outside this host and outside the backup location? Type "yes": '
    read -r answer
    [ "$answer" = yes ] || die "no backup made. Store the key first (docs/runbook.md, \"Secrets\")."
else
    die "confirm that PGW_KEK is stored separately: pass --kek-copy FILE or --kek-stored"
fi

# ---------------------------------------------------------------------------------------------
# Dump
# ---------------------------------------------------------------------------------------------
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
target="$backup_root/$stamp"
mkdir -p "$target"
finished=no
cleanup() {
    if [ "$finished" != yes ]; then
        warn "the backup did not finish; removing $target"
        rm -rf "$target"
    fi
}
trap cleanup EXIT

info "PostgreSQL -> $target/postgres.dump"
# The gateway's database is pacod_gw (platform/postgres/gw-db.sh), not the image's POSTGRES_DB.
# shellcheck disable=SC2016  # expanded by the shell inside the container
compose exec -T pacod-postgres sh -c \
    'pg_dump -U "$POSTGRES_USER" -d pacod_gw --format=custom' >"$target/postgres.dump"
# A dump that cannot be listed cannot be restored.
compose exec -T pacod-postgres pg_restore --list <"$target/postgres.dump" >/dev/null ||
    die "pg_restore cannot read the dump"
postgres_version="$(pg_sql postgres 'SHOW server_version')"

# pacod-id's database, in the same PostgreSQL (docs/runbook.md, section 16).
pacod_id="not included"
if [ "$(pg_sql postgres "SELECT count(*) FROM pg_database WHERE datname = 'pacod_id'")" = "1" ]; then
    info "PostgreSQL (pacod-id) -> $target/pacod-id.dump"
    # shellcheck disable=SC2016  # expanded by the shell inside the container
    compose exec -T pacod-postgres sh -c \
        'pg_dump -U "$POSTGRES_USER" -d pacod_id --format=custom' >"$target/pacod-id.dump"
    compose exec -T pacod-postgres pg_restore --list <"$target/pacod-id.dump" >/dev/null ||
        die "pg_restore cannot read pacod-id's dump"
    pacod_id="included"
fi

clickhouse_version="not included"
if [ "$with_clickhouse" = yes ]; then
    ch_database="$(database_name gw-clickhouse CLICKHOUSE_DB)"
    clickhouse_version="$(ch_sql 'SELECT version()')"
    if [ "$(ch_sql "SELECT count() FROM system.databases WHERE name = '$ch_database'")" = "0" ]; then
        warn "ClickHouse has no database \"$ch_database\" yet; nothing to back up there"
        clickhouse_version="$clickhouse_version (database \"$ch_database\" did not exist)"
    else
        info "ClickHouse -> $target/clickhouse.zip"
        archive="pacod-gw-$stamp.zip"
        ch_sql "BACKUP DATABASE \`$ch_database\` TO File('$archive')" >/dev/null
        compose exec -T gw-clickhouse cat "/var/lib/clickhouse/backups/$archive" >"$target/clickhouse.zip"
        compose exec -T gw-clickhouse rm -f "/var/lib/clickhouse/backups/$archive"
        [ -s "$target/clickhouse.zip" ] || die "the ClickHouse backup is empty"
    fi
fi

# ---------------------------------------------------------------------------------------------
# Manifest and checksums
# ---------------------------------------------------------------------------------------------
{
    echo "pacod-gw backup"
    echo "created_utc: $stamp"
    echo "host: $(hostname)"
    echo "postgres_version: $postgres_version"
    echo "pacod_id: $pacod_id"
    echo "clickhouse_version: $clickhouse_version"
    echo "images:"
    compose images 2>/dev/null | sed 's/^/  /' || true
    echo
    echo "# The key-encryption keys the stack ran with. The keys are not in this backup;"
    echo "# restore.sh compares these fingerprints with the keys of the stack it restores into."
    printf '%s\n' "$keys" | while read -r kind id print; do
        echo "kek: $kind $id $print"
    done
} >"$target/manifest.txt"

(
    cd "$target"
    files="postgres.dump manifest.txt"
    [ -f clickhouse.zip ] && files="$files clickhouse.zip"
    [ -f pacod-id.dump ] && files="$files pacod-id.dump"
    # shellcheck disable=SC2086  # the list is split on purpose
    sha256 $files >SHA256SUMS
)

finished=yes
info "Backup complete: $target"
du -sh "$target" | awk '{ print "Size: " $1 }'

if [ -n "$keep" ] && [ "$keep" -gt 0 ]; then
    # Backup directories are named by their time stamp, so name order is age order.
    find "$backup_root" -mindepth 1 -maxdepth 1 -type d -name '20*T*Z' | sort -r | tail -n "+$((keep + 1))" |
        while read -r old; do
            info "Removing old backup $old"
            rm -rf "$old"
        done
fi

info "Copy the directory off this host. Restore with: deploy/scripts/restore.sh $target"
