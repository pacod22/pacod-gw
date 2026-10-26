# shellcheck shell=bash
# Shared by backup.sh and restore.sh. Sourced, not run.
#
# Works with bash 3.2 (macOS) and newer. Needs docker with the compose plugin, and
# sha256sum or shasum.

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export DEPLOY_DIR

info() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# docker compose for this stack. COMPOSE_FILE, when set, names the files (for example the
# production files plus the development overrides); otherwise the app file and the standalone
# file, which holds PostgreSQL and pacod-db-init.
compose() {
    if [ -n "${COMPOSE_FILE:-}" ]; then
        docker compose "$@"
    else
        docker compose -f "$DEPLOY_DIR/docker-compose.yml" -f "$DEPLOY_DIR/docker-compose.standalone.yml" "$@"
    fi
}

require_tools() {
    command -v docker >/dev/null 2>&1 || die "docker is not installed"
    docker compose version >/dev/null 2>&1 || die "the docker compose plugin is not installed"
    if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
        die "neither sha256sum nor shasum is installed"
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$@"
    else
        shasum -a 256 "$@"
    fi
}

# Sixteen hex characters that identify a key without revealing it.
fingerprint() {
    printf '%s' "$1" | sha256 - | cut -c1-16
}

service_running() {
    compose ps --status running --services 2>/dev/null | grep -qx "$1"
}

# Prints one line per key-encryption key the stack is configured with:
#   current <id> <fingerprint>
#   old <id> <fingerprint>
# The keys themselves are never printed.
#
#   kek_fingerprints running   the keys of the running core container, pacod-gw (what the data in the
#                              databases was written under); falls back to "config"
#   kek_fingerprints config    the keys as compose resolves them now from deploy/.env (what
#                              the core will get at its next start)
kek_fingerprints() {
    local source="$1" env_dump="" container="" kek kek_id kek_old pair
    if [ "$source" = running ]; then
        container="$(compose ps -q pacod-gw 2>/dev/null | head -n 1 || true)"
    fi
    if [ -n "$container" ]; then
        env_dump="$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$container")"
    else
        # The gw-preflight service receives the same PGW_KEK, PGW_KEK_ID and PGW_KEK_OLD as the core.
        env_dump="$(compose run --rm --no-deps -T gw-preflight sh -c 'env' 2>/dev/null || true)"
    fi
    kek="$(printf '%s\n' "$env_dump" | sed -n 's/^PGW_KEK=//p' | head -n 1)"
    kek_id="$(printf '%s\n' "$env_dump" | sed -n 's/^PGW_KEK_ID=//p' | head -n 1)"
    kek_old="$(printf '%s\n' "$env_dump" | sed -n 's/^PGW_KEK_OLD=//p' | head -n 1)"
    [ -n "$kek" ] || return 1
    printf 'current %s %s\n' "${kek_id:-default}" "$(fingerprint "$kek")"
    local old_ifs="$IFS"
    IFS=','
    for pair in $kek_old; do
        IFS="$old_ifs"
        pair="$(printf '%s' "$pair" | tr -d ' ')"
        case "$pair" in
            *=*) printf 'old %s %s\n' "${pair%%=*}" "$(fingerprint "${pair#*=}")" ;;
        esac
    done
    IFS="$old_ifs"
}

# The single-quoted commands in this file and in the scripts are expanded by the shell
# inside the container, where the database user and password are set, not by this one.

# Runs a statement in PostgreSQL as its superuser, against the given database.
pg_sql() {
    local database="$1" statement="$2"
    # shellcheck disable=SC2016
    compose exec -T pacod-postgres sh -c \
        'psql -X -q -A -t -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$1" -c "$2"' sh "$database" "$statement"
}

# The scripts reach PostgreSQL as its superuser, POSTGRES_USER of the pacod-postgres container
# ("postgres"). An installation made while pacod_gw was the superuser has no such role until
# demote-db-role.sh has run.
require_pg_superuser() {
    pg_sql postgres 'SELECT 1' >/dev/null 2>&1 ||
        die "cannot reach PostgreSQL as its superuser. In an installation made while pacod_gw was PostgreSQL's superuser, run deploy/scripts/demote-db-role.sh first (docs/runbook.md, section 6)."
}

# Runs a statement in ClickHouse as the stack's user.
ch_sql() {
    # shellcheck disable=SC2016
    compose exec -T gw-clickhouse sh -c \
        'clickhouse-client --user "$CLICKHOUSE_USER" --password "$CLICKHOUSE_PASSWORD" --query "$1"' sh "$1"
}

# The database names come from the containers' own settings and are checked before they are
# put into a statement.
database_name() {
    local service="$1" variable="$2" name
    name="$(compose exec -T "$service" sh -c "printf '%s' \"\$$variable\"")"
    case "$name" in
        '' | *[!A-Za-z0-9_]* | [0-9]*) die "unexpected database name \"$name\" in service $service" ;;
    esac
    printf '%s' "$name"
}
