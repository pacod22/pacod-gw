#!/usr/bin/env bash
# Runs a gateway core for local development against the test PostgreSQL container
# (pgw-test-pg, port 25433). Development secrets only: never use these values elsewhere.
#
#   scripts/dev-core.sh                 # builds and runs the current code
#   PGW_BIN=path/to/binary scripts/dev-core.sh   # runs a given binary
set -euo pipefail
cd "$(dirname "$0")/.."

export DATABASE_URL="${DATABASE_URL:-postgres://postgres:test@localhost:25433/gw_ui}"
export PGW_KEK="${PGW_KEK:-ZGV2LW9ubHkta2VrLTAxMjM0NTY3ODlhYmNkZWYwMTI=}"
export PGW_KEY_PEPPER="${PGW_KEY_PEPPER:-dev-only-pepper-0123456789abcdef}"
export PGW_ADMIN_EMAIL="${PGW_ADMIN_EMAIL:-admin@example.test}"
export PGW_ADMIN_PASSWORD="${PGW_ADMIN_PASSWORD:-correct horse battery}"
export PGW_LISTEN="${PGW_LISTEN:-127.0.0.1:28080}"
export PGW_ADMIN_LISTEN="${PGW_ADMIN_LISTEN:-127.0.0.1:28081}"
export PGW_METRICS_LISTEN="${PGW_METRICS_LISTEN:-127.0.0.1:29100}"
export PGW_EGRESS_ALLOW_PRIVATE="${PGW_EGRESS_ALLOW_PRIVATE:-127.0.0.0/8,localhost}"
export PGW_SPOOL_DIR="${PGW_SPOOL_DIR:-target/dev-spool}"
export PGW_CONFIG="${PGW_CONFIG:-/dev/null}"

if [[ -n "${PGW_BIN:-}" ]]; then
  exec "$PGW_BIN" serve
fi
exec cargo run -q -p pacod-gw-core -- serve
