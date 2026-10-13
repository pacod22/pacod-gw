#!/bin/sh
# Run by the "preflight" service of deploy/docker-compose.yml before anything else starts.
# It stops the start when a secret is missing, too short or of the wrong shape, and names
# every problem at once. It prints no secret.
#
# PREFLIGHT_PROFILE=node (deploy/node/docker-compose.yml) checks what a gateway on another
# machine needs instead: the secrets it shares with the central host and the addresses of
# the central services.
#
# The script runs in BusyBox, so it keeps to POSIX sh.

set -eu

problems=0

problem() {
    echo "preflight: $1" >&2
    problems=$((problems + 1))
}

# need NAME MIN_LENGTH HINT: HINT says where the value comes from.
need() {
    eval "value=\${$1:-}"
    # shellcheck disable=SC2154  # "value" is assigned by the eval above
    if [ -z "$value" ]; then
        problem "$1 is not set. $3"
    elif [ "${#value}" -lt "$2" ]; then
        problem "$1 is shorter than $2 characters. $3"
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

warning() {
    echo "preflight: warning: $1" >&2
}

if [ "${PREFLIGHT_PROFILE:-}" = "node" ]; then
    # Shared with the central host: copy them from its deploy/.env, do not generate new ones.
    shared="Copy it from the central host's deploy/.env; it must be the same everywhere."
    need PGW_KEK 44 "$shared"
    # Every sealed credential names the key it was sealed under: with another id the
    # gateway cannot read one.
    need PGW_KEK_ID 1 "Copy it from the central host's deploy/.env (\"default\" if it is not set there)."
    need PGW_KEY_PEPPER 16 "$shared"
    case "${DATABASE_URL:-}" in
        postgres://*|postgresql://*) ;;
        "") problem "DATABASE_URL is not set: the central PostgreSQL, postgres://pacod_gw:PASSWORD@HOST:5432/pacod_gw" ;;
        *) problem "DATABASE_URL must start with postgres://" ;;
    esac
    case "${PGW_INSTANCE_ID:-}" in
        "") problem "PGW_INSTANCE_ID is not set: give every gateway a name of its own, for example gateway-eu-1" ;;
        [A-Za-z0-9]*) case "$PGW_INSTANCE_ID" in
                *[!A-Za-z0-9._-]*) problem "PGW_INSTANCE_ID may contain only letters, digits and . _ -" ;;
            esac ;;
        *) problem "PGW_INSTANCE_ID must start with a letter or a digit" ;;
    esac
    if [ -n "${PGW_ANALYTICS_URL:-}" ]; then
        need PGW_INTERNAL_TOKEN 16 "$shared"
    else
        warning "PGW_ANALYTICS_URL is not set: this gateway records no usage events, so its calls are missing from every report"
    fi
    if [ -z "${PGW_TRUSTED_PROXIES:-}" ]; then
        warning "PGW_TRUSTED_PROXIES is not set: behind the central Caddy every client looks like one address to the bad-key throttle"
    fi
    if [ "${PGW_NODE_BIND:-}" = "0.0.0.0" ]; then
        warning "PGW_NODE_BIND is 0.0.0.0: the data plane and the metrics are open on every interface; prefer the private address"
    fi
else
    need POSTGRES_PASSWORD 16 "Generate it with: openssl rand -hex 24"
    need CLICKHOUSE_PASSWORD 16 "Generate it with: openssl rand -hex 24"
    need PGW_KEK 44 "Generate it with: openssl rand -base64 32"
    need PGW_KEY_PEPPER 16 "Generate it with: openssl rand -base64 32"
    need PGW_INTERNAL_TOKEN 16 "Generate it with: openssl rand -hex 32"
    need GRAFANA_ADMIN_PASSWORD 12 "Generate it with: openssl rand -base64 18"

    url_safe POSTGRES_PASSWORD
    url_safe CLICKHOUSE_PASSWORD

    # pacod-id (the "id" service), which runs whoever signs people in.
    case "${PGW_AUTH:-local}" in
        local|oidc) ;;
        *) problem "PGW_AUTH must be local or oidc, not \"$PGW_AUTH\"" ;;
    esac
    [ -n "${PGW_ID_HOST:-}" ] || problem "PGW_ID_HOST is not set: pacod-id's host name, such as id.example.com"
    need PID_DB_PASSWORD 16 "Generate it with: openssl rand -hex 24"
    url_safe PID_DB_PASSWORD
    need PID_KEK 44 "Generate it with: openssl rand -base64 32"
    need PID_TOKEN_PEPPER 32 "Generate it with: openssl rand -hex 32"
    need PGW_ID_CLIENT_SECRET 32 "Generate it with: openssl rand -hex 32"
    need PGW_UI_CLIENT_SECRET 32 "Generate it with: openssl rand -hex 32"
    need TESTER_ID_CLIENT_SECRET 32 "Generate it with: openssl rand -hex 32"
    need PGW_UI_SESSION_SECRET 32 "Generate it with: openssl rand -hex 32"
fi

if [ -n "${PGW_KEK:-}" ]; then
    kek_ok PGW_KEK "$PGW_KEK"
fi
if [ -n "${PID_KEK:-}" ]; then
    kek_ok PID_KEK "$PID_KEK"
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
    if [ "${PREFLIGHT_PROFILE:-}" = "node" ]; then
        echo "preflight: $problems problem(s). Fix deploy/node/.env (see deploy/node/.env.example) and start again." >&2
    else
        echo "preflight: $problems problem(s). Fix deploy/.env (see deploy/.env.example) and start again." >&2
    fi
    exit 1
fi

echo "preflight: all required secrets are set"
