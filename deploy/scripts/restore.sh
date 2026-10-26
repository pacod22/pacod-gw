#!/usr/bin/env bash
# Restores a backup made by backup.sh into the pacod-gw compose stack.
#
# Meant for a fresh stack: deploy/.env is in place (with the key-encryption key the backup
# was made under) and nothing has been started yet. The script then
#   1. checks the backup's checksums,
#   2. checks that the stack is configured with the key-encryption key of the backup,
#   3. starts PostgreSQL and ClickHouse only,
#   4. refuses to continue if either database already holds tables (unless --force); up to
#      here nothing has been changed or stopped,
#   5. stops pacod-gw, the gateways of the scale layout, gw-analytics, gw-console and pacod-id
#      if they run,
#      and recreates the databases from the backup (pacod-id's too, when the backup has it;
#      it needs the same PID_KEK as before, which this script does not check), each owned by
#      its own role, whichever role owned it when the backup was made,
#   6. starts the whole stack. The core and the analytics service then bring the schemas up
#      to their own version, so a backup from an older release can be restored.
#
# Usage: deploy/scripts/restore.sh BACKUP_DIR [options]
#   --force            Replace the databases even if they hold data. Everything in them that
#                      is newer than the backup is lost.
#   --skip-kek-check   Restore although the key-encryption key cannot be confirmed. The
#                      provider credentials stay unreadable until the right key is set.
#   --skip-clickhouse  Restore PostgreSQL only.
#   --no-start         Leave the stack stopped after the restore.
#   -h, --help         Show this text.
#
# With the scale or hub layout, set COMPOSE_FILE to the same files the stack runs with
# (deploy/docker-compose.yml:deploy/docker-compose.standalone.yml:deploy/docker-compose.scale.yml:...). Gateways on other
# machines are not stopped by this script: stop them first ("docker compose -f
# deploy/node/docker-compose.yml stop" on each) and start them again after the restore.

set -euo pipefail

# shellcheck source=deploy/scripts/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

source_dir=""
force=no
check_kek=yes
with_clickhouse=yes
start_stack=yes

while [ $# -gt 0 ]; do
    case "$1" in
        --force) force=yes ;;
        --skip-kek-check) check_kek=no ;;
        --skip-clickhouse) with_clickhouse=no ;;
        --no-start) start_stack=no ;;
        -h | --help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        -*) die "unknown option $1 (see --help)" ;;
        *)
            [ -z "$source_dir" ] || die "only one backup directory can be given"
            source_dir="$1"
            ;;
    esac
    shift
done

[ -n "$source_dir" ] || die "usage: restore.sh BACKUP_DIR [--force] [--skip-clickhouse] [--no-start]"
[ -d "$source_dir" ] || die "$source_dir is not a directory"
source_dir="$(cd "$source_dir" && pwd -P)"

require_tools

# ---------------------------------------------------------------------------------------------
# 1. The backup itself
# ---------------------------------------------------------------------------------------------
for file in postgres.dump manifest.txt SHA256SUMS; do
    [ -f "$source_dir/$file" ] || die "$source_dir/$file is missing; this is not a complete backup"
done
info "Checking checksums"
(cd "$source_dir" && sha256 -c SHA256SUMS >/dev/null) || die "the backup is damaged: checksums do not match"

with_pacod_id=no
[ -f "$source_dir/pacod-id.dump" ] && with_pacod_id=yes

if [ "$with_clickhouse" = yes ] && [ ! -f "$source_dir/clickhouse.zip" ]; then
    warn "the backup has no clickhouse.zip; restoring PostgreSQL only"
    with_clickhouse=no
fi

# ---------------------------------------------------------------------------------------------
# 2. The key-encryption key
# ---------------------------------------------------------------------------------------------
backup_current="$(awk '$1 == "kek:" && $2 == "current" { print $4 }' "$source_dir/manifest.txt")"
backup_current_id="$(awk '$1 == "kek:" && $2 == "current" { print $3 }' "$source_dir/manifest.txt")"

if [ "$check_kek" = yes ]; then
    [ -n "$backup_current" ] || die "the manifest names no key-encryption key; use --skip-kek-check to restore anyway"
    configured="$(kek_fingerprints config)" ||
        die "PGW_KEK is not set. Put the key the backup was made under into deploy/.env (id \"$backup_current_id\", fingerprint $backup_current)."
    if ! printf '%s\n' "$configured" | awk '{ print $3 }' | grep -qx "$backup_current"; then
        die "the stack is not configured with the key-encryption key of this backup (id \"$backup_current_id\", fingerprint $backup_current). Set it as PGW_KEK, or add it to PGW_KEK_OLD, in deploy/.env. The key is not in the backup; it comes from wherever it was stored separately."
    fi
    info "The key-encryption key of the backup (id \"$backup_current_id\") is configured."
    # Keys that were only kept for older records at backup time.
    awk '$1 == "kek:" && $2 == "old" { print $3, $4 }' "$source_dir/manifest.txt" | while read -r id print; do
        if ! printf '%s\n' "$configured" | awk '{ print $3 }' | grep -qx "$print"; then
            warn "the older key \"$id\" ($print) was configured at backup time and is not configured now; records still encrypted under it will be unreadable"
        fi
    done
else
    warn "not checking the key-encryption key"
fi

# ---------------------------------------------------------------------------------------------
# 3. Databases up
# ---------------------------------------------------------------------------------------------
if [ "$with_clickhouse" = yes ]; then
    info "Starting PostgreSQL and ClickHouse"
    compose up -d --wait pacod-postgres gw-clickhouse
else
    info "Starting PostgreSQL"
    compose up -d --wait pacod-postgres
fi
require_pg_superuser

# ---------------------------------------------------------------------------------------------
# 4. Refuse to overwrite data by accident
# ---------------------------------------------------------------------------------------------
# The gateway's database is pacod_gw (platform/postgres/gw-db.sh); on a fresh host it does not
# exist yet, which counts as empty.
pg_tables="$(pg_sql pacod_gw "SELECT count(*) FROM information_schema.tables WHERE table_schema NOT IN ('pg_catalog', 'information_schema')" 2>/dev/null || echo 0)"
if [ "$with_pacod_id" = yes ] &&
    [ "$(pg_sql postgres "SELECT count(*) FROM pg_database WHERE datname = 'pacod_id'")" = "1" ]; then
    pid_tables="$(pg_sql pacod_id "SELECT count(*) FROM information_schema.tables WHERE table_schema NOT IN ('pg_catalog', 'information_schema')" 2>/dev/null || echo 0)"
    pg_tables=$((pg_tables + pid_tables))
fi
ch_tables=0
if [ "$with_clickhouse" = yes ]; then
    ch_database="$(database_name gw-clickhouse CLICKHOUSE_DB)"
    ch_tables="$(ch_sql "SELECT count() FROM system.tables WHERE database = '$ch_database'")"
fi

if [ "$pg_tables" != "0" ] || [ "$ch_tables" != "0" ]; then
    if [ "$force" != yes ]; then
        die "the stack already holds data (PostgreSQL: $pg_tables tables, ClickHouse: $ch_tables tables). Nothing was changed. Run again with --force to replace it with the backup."
    fi
    warn "replacing existing data (PostgreSQL: $pg_tables tables, ClickHouse: $ch_tables tables)"
fi

# ---------------------------------------------------------------------------------------------
# 5. Restore, with nothing else writing
# ---------------------------------------------------------------------------------------------
# The gateways of the scale layout write limit counters and their instance rows.
gateways="$(compose config --services 2>/dev/null | grep -E '^gw-gateway-' | tr '\n' ' ' || true)"
info "Stopping pacod-gw ${gateways}gw-analytics, gw-console and pacod-id"
warn "gateways on other machines must be stopped too; this script cannot reach them"
# shellcheck disable=SC2086  # one word per service name
compose stop pacod-gw $gateways gw-analytics gw-console pacod-id >/dev/null 2>&1 || true

pg_sql postgres "DROP DATABASE IF EXISTS pacod_gw WITH (FORCE)"
[ "$with_pacod_id" = no ] || pg_sql postgres "DROP DATABASE IF EXISTS pacod_id WITH (FORCE)"
# The roles and empty databases owned by them (pacod-db-init runs platform/postgres/gw-db.sh and
# postgres/id-db.sh), then the tables, created as those roles. A backup made while pacod_gw was
# PostgreSQL's superuser names it as the owner; --no-owner leaves that out.
compose run --rm --no-deps pacod-db-init >/dev/null

info "Restoring PostgreSQL database \"pacod_gw\""
# shellcheck disable=SC2016  # expanded by the shell inside the container
compose exec -T pacod-postgres sh -c \
    'pg_restore -U "$POSTGRES_USER" -d pacod_gw --no-owner --no-acl --role=pacod_gw --exit-on-error' \
    <"$source_dir/postgres.dump"

if [ "$with_pacod_id" = yes ]; then
    info "Restoring PostgreSQL database \"pacod_id\""
    # shellcheck disable=SC2016  # expanded by the shell inside the container
    compose exec -T pacod-postgres sh -c \
        'pg_restore -U "$POSTGRES_USER" -d pacod_id --no-owner --no-acl --role=pacod_id --exit-on-error' \
        <"$source_dir/pacod-id.dump"
fi

if [ "$with_clickhouse" = yes ]; then
    info "Restoring ClickHouse database \"$ch_database\""
    archive="pacod-gw-restore-$(date -u +%Y%m%dT%H%M%SZ).zip"
    # shellcheck disable=SC2016  # expanded by the shell inside the container
    compose exec -T --user clickhouse gw-clickhouse sh -c \
        'mkdir -p /var/lib/clickhouse/backups && cat > "/var/lib/clickhouse/backups/$1"' sh "$archive" \
        <"$source_dir/clickhouse.zip"
    ch_sql "DROP DATABASE IF EXISTS \`$ch_database\` SYNC"
    ch_sql "RESTORE DATABASE \`$ch_database\` FROM File('$archive')" >/dev/null
    compose exec -T --user clickhouse gw-clickhouse rm -f "/var/lib/clickhouse/backups/$archive"
fi

# ---------------------------------------------------------------------------------------------
# 6. Start
# ---------------------------------------------------------------------------------------------
if [ "$start_stack" = yes ]; then
    info "Starting the stack"
    compose up -d --wait
    info "Restore complete. Log in to the console and run a provider probe: it fails if the"
    info "key-encryption key does not belong to the restored credentials."
else
    info "Restore complete. The stack is not started (--no-start)."
fi
