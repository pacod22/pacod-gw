#!/bin/sh
# pacod-id's role and database on the gateway's own PostgreSQL (pacod-postgres), next to the
# gateway's: a role "pacod_id" that is not a superuser and owns the database "pacod_id". The same
# script as pacod-id's platform/postgres/id-db.sh, for the pacod-id the gateway bundles; the full
# stack (pacod-demo) runs pacod-id's own copy and not this one.
#
# pacod-db-init (docker-compose.standalone.yml) runs it at every start, after
# platform/postgres/gw-db.sh and before pacod-id. Safe to run again: it creates what is missing
# and sets the role's password to PID_DB_PASSWORD. By hand, against a PostgreSQL of your own, as
# a superuser, with psql 15 or newer:
#
#   PGHOST=HOST PGUSER=pacod_gw PGPASSWORD=... PID_DB_PASSWORD=... sh deploy/postgres/id-db.sh
#
# The connection comes from libpq's PG* variables and the new password from the environment
# (psql's \getenv), so neither is on a command line where the process list would show it. It
# prints no secret.

set -eu

: "${PGHOST:?the PostgreSQL host, with PGUSER and PGPASSWORD of a superuser}"
: "${PID_DB_PASSWORD:?the password for the role pacod_id}"

psql -v ON_ERROR_STOP=1 -q -d postgres <<'SQL'
\getenv pw PID_DB_PASSWORD
SELECT format('CREATE ROLE pacod_id LOGIN PASSWORD %L', :'pw')
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'pacod_id') \gexec
SELECT format('ALTER ROLE pacod_id LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD %L', :'pw')
 WHERE current_user <> 'pacod_id' \gexec
SELECT 'CREATE DATABASE pacod_id OWNER pacod_id'
 WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'pacod_id') \gexec
REVOKE ALL ON DATABASE pacod_id FROM PUBLIC;
GRANT CONNECT, CREATE, TEMPORARY ON DATABASE pacod_id TO pacod_id;
SQL

echo "id-db: the role and the database pacod_id are in place"
