#!/bin/sh
# The database and role of pacod-id (identity concept 16.3), next to the gateway's in the same
# PostgreSQL: a role "pacod_id" that is not a superuser and owns the database "pacod_id".
# Safe to run again: it creates what is missing and sets the role's password to PID_DB_PASSWORD.
#
# The "id-db" service of docker-compose.yml runs it before pacod-id starts. By hand, against a
# PostgreSQL of your own:
#
#   POSTGRES_URL=postgres://pacod_gw:PASSWORD@HOST:5432/pacod_gw PID_DB_PASSWORD=... \
#     sh deploy/scripts/create-id-role.sh
#
# It prints no secret.

set -eu

: "${POSTGRES_URL:?the URL of the gateway superuser, postgres://pacod_gw:PASSWORD@postgres:5432/pacod_gw}"
: "${PID_DB_PASSWORD:?the password for the role pacod_id}"

psql "$POSTGRES_URL" -v ON_ERROR_STOP=1 -q -v pw="$PID_DB_PASSWORD" <<'SQL'
SELECT format('CREATE ROLE pacod_id LOGIN PASSWORD %L', :'pw')
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'pacod_id') \gexec
SELECT format('ALTER ROLE pacod_id LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD %L', :'pw') \gexec
SELECT 'CREATE DATABASE pacod_id OWNER pacod_id'
 WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'pacod_id') \gexec
REVOKE ALL ON DATABASE pacod_id FROM PUBLIC;
GRANT CONNECT, CREATE, TEMPORARY ON DATABASE pacod_id TO pacod_id;
SQL

echo "create-id-role: the role and the database pacod_id are in place"
