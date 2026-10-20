#!/bin/sh
# pacod-gw's role and database on the shared PostgreSQL (pacod-postgres): a role "pacod_gw" that
# is not a superuser and owns the database "pacod_gw". The core creates its tables there
# (migrations run at start); none of them needs a superuser.
#
# pacod-db-init runs this at every start, before the core, as a superuser through libpq's PG*
# variables (PGHOST, PGUSER, PGPASSWORD). Safe to run again: it creates what is missing and sets
# the role's password to GW_DB_PASSWORD. The password is read with psql's \getenv, so it is on no
# command line, and nothing secret is printed. The same file serves the gateway's own stack
# (deploy/docker-compose.standalone.yml) and the full stack (pacod-demo).
#
# In the gateway's own stack, and in every installation made before this file, pacod_gw is the
# superuser the PostgreSQL image created (POSTGRES_USER), and pacod-db-init connects as it. The
# role is then left as it is: a superuser cannot take its own rights away, and its password stays
# POSTGRES_PASSWORD.

set -eu

: "${PGHOST:?the PostgreSQL host, with PGUSER and PGPASSWORD of a superuser}"
: "${GW_DB_PASSWORD:?the password for the role pacod_gw}"

psql -v ON_ERROR_STOP=1 -q -d postgres <<'SQL'
\getenv pw GW_DB_PASSWORD
SELECT format('CREATE ROLE pacod_gw LOGIN PASSWORD %L', :'pw')
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'pacod_gw') \gexec
SELECT format('ALTER ROLE pacod_gw LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD %L', :'pw')
 WHERE current_user <> 'pacod_gw' \gexec
SELECT 'CREATE DATABASE pacod_gw OWNER pacod_gw'
 WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'pacod_gw') \gexec
REVOKE ALL ON DATABASE pacod_gw FROM PUBLIC;
GRANT CONNECT, CREATE, TEMPORARY ON DATABASE pacod_gw TO pacod_gw;
SQL

echo "gw-db: the role and the database pacod_gw are in place"
