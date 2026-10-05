#!/bin/sh
# Run by the "preflight" service of deploy/docker-compose.yml before anything else starts.
# It stops the start when a secret is missing, too short or of the wrong shape, and names
# every problem at once. It prints no secret.
#
# The script runs in BusyBox, so it keeps to POSIX sh.

set -eu

problems=0

problem() {
    echo "preflight: $1" >&2
    problems=$((problems + 1))
}

# need NAME MIN_LENGTH HOW_TO_GENERATE
need() {
    eval "value=\${$1:-}"
    # shellcheck disable=SC2154  # "value" is assigned by the eval above
    if [ -z "$value" ]; then
        problem "$1 is not set. Generate it with: $3"
    elif [ "${#value}" -lt "$2" ]; then
        problem "$1 is shorter than $2 characters. Generate it with: $3"
    fi
}

# url_safe NAME: the value is placed in a URL or in an XML file by the database images.
url_safe() {
    eval "value=\${$1:-}"
    case "$value" in
        *[!A-Za-z0-9._~-]*)
            problem "$1 may contain only letters, digits and the characters . _ ~ -" ;;
    esac
}

# The key-encryption key must decode to exactly 32 bytes.
kek_ok() {
    bytes=$(printf '%s' "$2" | base64 -d 2>/dev/null | wc -c | tr -d ' ')
    if [ "$bytes" != "32" ]; then
        problem "$1 must be base64 of 32 bytes. Generate it with: openssl rand -base64 32"
    fi
}

need POSTGRES_PASSWORD 16 "openssl rand -hex 24"
need CLICKHOUSE_PASSWORD 16 "openssl rand -hex 24"
need PGW_KEK 44 "openssl rand -base64 32"
need PGW_KEY_PEPPER 16 "openssl rand -base64 32"
need PGW_INTERNAL_TOKEN 16 "openssl rand -hex 32"
need GRAFANA_ADMIN_PASSWORD 12 "openssl rand -base64 18"

url_safe POSTGRES_PASSWORD
url_safe CLICKHOUSE_PASSWORD

if [ -n "${PGW_KEK:-}" ]; then
    kek_ok PGW_KEK "$PGW_KEK"
fi

# Older keys, kept so that records encrypted before a rotation stay readable: id=key,id=key
if [ -n "${PGW_KEK_OLD:-}" ]; then
    old_ifs=$IFS
    IFS=,
    for pair in $PGW_KEK_OLD; do
        IFS=$old_ifs
        pair=$(printf '%s' "$pair" | tr -d ' ')
        [ -n "$pair" ] || continue
        case "$pair" in
            *=*) kek_ok "PGW_KEK_OLD entry \"${pair%%=*}\"" "${pair#*=}" ;;
            *) problem "PGW_KEK_OLD entries look like id=base64key" ;;
        esac
    done
    IFS=$old_ifs
fi

if [ "$problems" -gt 0 ]; then
    echo "preflight: $problems problem(s). Fix deploy/.env (see deploy/.env.example) and start again." >&2
    exit 1
fi

echo "preflight: all required secrets are set"
