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
# In a gateway's own stack installed while pacod_gw was PostgreSQL's superuser, pacod_gw is the
# role the PostgreSQL image created first (oid 10). PostgreSQL keeps that role a superuser
# whatever it is told, so the script refuses it: deploy/scripts/demote-db-role.sh renames it to
# "postgres" and makes pacod_gw anew, once (docs/runbook.md, section 6).

set -eu

: "${PGHOST:?the PostgreSQL host, with PGUSER and PGPASSWORD of a superuser}"
: "${GW_DB_PASSWORD:?the password for the role pacod_gw}"

psql -v ON_ERROR_STOP=1 -q -d postgres <<'SQL'
DO $$
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE oid = 10 AND rolname = 'pacod_gw') THEN
    RAISE EXCEPTION 'pacod_gw is PostgreSQL''s bootstrap superuser here; run deploy/scripts/demote-db-role.sh once (docs/runbook.md, section 6)';
  END IF;
END
$$;
\getenv pw GW_DB_PASSWORD
SELECT format('CREATE ROLE pacod_gw LOGIN PASSWORD %L', :'pw')
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'pacod_gw') \gexec
SELECT format('ALTER ROLE pacod_gw LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD %L', :'pw') \gexec
SELECT 'CREATE DATABASE pacod_gw OWNER pacod_gw'
 WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'pacod_gw') \gexec
REVOKE ALL ON DATABASE pacod_gw FROM PUBLIC;
GRANT CONNECT, CREATE, TEMPORARY ON DATABASE pacod_gw TO pacod_gw;
SQL

echo "gw-db: the role and the database pacod_gw are in place"
