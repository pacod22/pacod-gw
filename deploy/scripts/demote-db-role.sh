#!/usr/bin/env bash
# Makes the role pacod_gw an ordinary owner of its database, once, in an installation of the
# gateway's own stack made while pacod_gw was PostgreSQL's superuser (docs/runbook.md, section 6).
#
# There pacod_gw is the role the PostgreSQL image created first (oid 10), and PostgreSQL keeps
# that role a superuser whatever it is told. The script therefore renames it to "postgres", the
# superuser of a new installation, and makes pacod_gw anew: the owner of the database pacod_gw
# and of everything in it, and nothing more. Afterwards the installation is like a new one: the
# core can no longer read or change pacod-id's database.
#
# Before running it, in deploy/.env:
#   GW_DB_PASSWORD     the value POSTGRES_PASSWORD has now. The core, and the gateways on
#                      other machines, keep connecting with it.
#   POSTGRES_PASSWORD  a new value (openssl rand -hex 24), for the superuser alone. The core
#                      and the other machines have held the old one.
# and stop the gateways on other machines (deploy/node/).
#
# The script then
#   1. runs preflight, and finds out whether there is anything to do,
#   2. refuses, changing nothing, when POSTGRES_PASSWORD is still the superuser's password,
#   3. stops pacod-gw and the gateways of the scale layout, and restarts PostgreSQL with the new
#      settings,
#   4. refuses when anything else is still connected as the superuser (a gateway on another
#      machine, a psql session): such a session would keep the superuser's rights,
#   5. renames the superuser, sets its password to POSTGRES_PASSWORD, makes pacod_gw and gives
#      it the database and everything in it, in one transaction: it changes all or nothing,
#   6. starts the stack; pacod-db-init gives pacod_gw the password GW_DB_PASSWORD.
# Run again, it finds the work done and changes nothing.
#
# Usage: deploy/scripts/demote-db-role.sh [options]
#   --no-start   Leave the stack as it is afterwards: the gateways stay stopped.
#   -h, --help   Show this text.
#
# With the scale or hub layout, set COMPOSE_FILE to the same files the stack runs with
# (deploy/docker-compose.yml:deploy/docker-compose.standalone.yml:deploy/docker-compose.scale.yml:...).

set -euo pipefail

# shellcheck source=deploy/scripts/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

start_stack=yes

while [ $# -gt 0 ]; do
    case "$1" in
        --no-start) start_stack=no ;;
        -h | --help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
    shift
done

require_tools

# psql in the pacod-postgres container, over its socket, which the image trusts: as USER, against
# DATABASE, with the statements on standard input. The superuser's new password is read inside
# the container from its own POSTGRES_PASSWORD, so it is on no command line.
pg_as() {
    compose exec -T pacod-postgres psql -X -q -A -t -v ON_ERROR_STOP=1 -U "$1" -d "$2"
}

# ---------------------------------------------------------------------------------------------
# 1. Preflight, and what there is to do
# ---------------------------------------------------------------------------------------------
info "Checking deploy/.env (preflight)"
compose run --rm --no-deps -T gw-preflight >/dev/null ||
    die "preflight refused deploy/.env (above). GW_DB_PASSWORD must be the value POSTGRES_PASSWORD has now, and POSTGRES_PASSWORD a new one."

service_running pacod-postgres || compose up -d --wait --no-deps pacod-postgres

# Who oid 10 is: "postgres" once this script has run (or in a new installation), "pacod_gw"
# before. The role postgres does not exist before, so ask as it first, then as pacod_gw.
bootstrap="$(pg_as postgres postgres <<<'SELECT rolname FROM pg_roles WHERE oid = 10' 2>/dev/null || true)"
[ -n "$bootstrap" ] || bootstrap="$(pg_as pacod_gw postgres <<<'SELECT rolname FROM pg_roles WHERE oid = 10' 2>/dev/null || true)"

case "$bootstrap" in
    postgres)
        # A run that stopped after its transaction leaves the temporary role behind.
        pg_as postgres postgres <<<'SET client_min_messages = warning; DROP ROLE IF EXISTS pgw_demote' >/dev/null
        info "Nothing to do: PostgreSQL's superuser is \"postgres\", and pacod_gw is an ordinary role."
        exit 0
        ;;
    pacod_gw) ;;
    *) die "cannot reach PostgreSQL as \"postgres\" or as \"pacod_gw\"; see: docker compose logs pacod-postgres" ;;
esac

[ "$(pg_as pacod_gw postgres <<<"SELECT count(*) FROM pg_roles WHERE rolname = 'postgres'")" = "0" ] ||
    die "PostgreSQL already has a role \"postgres\" besides its superuser pacod_gw. Nothing was changed. Rename or drop that role first."
[ "$(pg_as pacod_gw postgres <<<"SELECT count(*) FROM pg_database WHERE datname = 'pacod_gw'")" = "1" ] ||
    die "PostgreSQL has no database pacod_gw. Nothing was changed."

# ---------------------------------------------------------------------------------------------
# 2. The superuser's password must change
# ---------------------------------------------------------------------------------------------
# pacod-db-init holds the new POSTGRES_PASSWORD and connects over the network, where PostgreSQL
# asks for passwords. Logging in as pacod_gw with it must fail.
info "Checking that POSTGRES_PASSWORD is a new one"
if login="$(compose run --rm --no-deps -T -e PGUSER=pacod_gw --entrypoint psql pacod-db-init \
    -X -q -A -t -d postgres -c 'SELECT 1' 2>&1)"; then
    die "POSTGRES_PASSWORD is still the password of pacod_gw, which the core and the gateways on other machines hold. Nothing was changed. Put that value into GW_DB_PASSWORD and generate a new POSTGRES_PASSWORD (openssl rand -hex 24)."
fi
case "$login" in
    *"password authentication failed"*) ;;
    *) die "could not check POSTGRES_PASSWORD; nothing was changed: $login" ;;
esac

# ---------------------------------------------------------------------------------------------
# 3. Nothing else writing, and PostgreSQL with the new settings
# ---------------------------------------------------------------------------------------------
gateways="$(compose config --services 2>/dev/null | grep -E '^gw-gateway-' | tr '\n' ' ' || true)"
info "Stopping pacod-gw ${gateways}"
# shellcheck disable=SC2086  # one word per service name
compose stop pacod-gw $gateways >/dev/null 2>&1 || true

# The new container has POSTGRES_USER=postgres, which the backup scripts use, and the new
# POSTGRES_PASSWORD, which step 5 reads. The data stays as it is.
info "Restarting pacod-postgres with the new settings"
compose up -d --wait --no-deps pacod-postgres

# ---------------------------------------------------------------------------------------------
# 4. Nobody else connected as the superuser
# ---------------------------------------------------------------------------------------------
others="$(pg_as pacod_gw postgres <<'SQL'
SELECT format('pid %s: %s from %s, database %s', pid, coalesce(nullif(application_name, ''), 'a client'),
              coalesce(host(client_addr), 'the local socket'), datname)
  FROM pg_stat_activity
 WHERE usesysid = 10 AND backend_type = 'client backend' AND pid <> pg_backend_pid();
SQL
)"
if [ -n "$others" ]; then
    printf '%s\n' "$others" | sed 's/^/  connected: /' >&2
    die "these sessions are connected as pacod_gw, the superuser, and would keep its rights. Nothing in PostgreSQL was changed; the gateways here are stopped. Stop the gateways on other machines (deploy/node/) and anything else above, then run this script again. A session whose client is gone but which still runs a statement can be ended with: docker compose ... exec pacod-postgres psql -U pacod_gw -d postgres -c 'SELECT pg_terminate_backend(<pid>)'"
fi

# ---------------------------------------------------------------------------------------------
# 5. The change, in one transaction
# ---------------------------------------------------------------------------------------------
# PostgreSQL refuses to rename the role a session is logged in as, so a temporary superuser does
# it. It has no password: only the container's socket lets it in.
pg_as pacod_gw postgres >/dev/null <<'SQL'
SELECT 'CREATE ROLE pgw_demote SUPERUSER LOGIN'
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'pgw_demote') \gexec
SQL

info "Renaming the superuser to \"postgres\", and giving the database pacod_gw to a new role pacod_gw"
pg_as pgw_demote pacod_gw >/dev/null <<'SQL'
BEGIN;
ALTER ROLE pacod_gw RENAME TO postgres;
\getenv pw POSTGRES_PASSWORD
SELECT format('ALTER ROLE postgres PASSWORD %L', :'pw') \gexec
-- Its password comes from pacod-db-init (platform/postgres/gw-db.sh) at the next start.
CREATE ROLE pacod_gw LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;
ALTER DATABASE pacod_gw OWNER TO pacod_gw;

-- PostgreSQL's own objects have oids below 16384, and an extension's stay with whoever installed
-- it: the core uses none. A table takes its indexes, its TOAST table and the sequences of its
-- columns along.
DO $$
DECLARE
  r record;
  left_over text;
BEGIN
  FOR r IN SELECT n.nspname FROM pg_namespace n
            WHERE n.nspowner = 10 AND n.nspname NOT LIKE 'pg\_%' AND n.nspname <> 'information_schema'
              AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_namespace'::regclass
                                                        AND d.objid = n.oid AND d.deptype = 'e')
  LOOP
    EXECUTE format('ALTER SCHEMA %I OWNER TO pacod_gw', r.nspname);
  END LOOP;

  FOR r IN SELECT c.oid::regclass AS name, c.relkind FROM pg_class c
            WHERE c.relowner = 10 AND c.oid >= 16384 AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S')
              AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
                                AND (d.deptype = 'e' OR (c.relkind = 'S' AND d.deptype IN ('a', 'i'))))
  LOOP
    EXECUTE format('ALTER %s %s OWNER TO pacod_gw',
                   CASE r.relkind WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW'
                                  WHEN 'f' THEN 'FOREIGN TABLE' WHEN 'S' THEN 'SEQUENCE' ELSE 'TABLE' END,
                   r.name);
  END LOOP;

  -- Row types of tables, array types and multiranges follow what they belong to.
  FOR r IN SELECT t.oid::regtype AS name, t.typtype FROM pg_type t
            WHERE t.typowner = 10 AND t.oid >= 16384 AND t.typtype IN ('c', 'd', 'e', 'r')
              AND (t.typtype <> 'c' OR (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c')
              AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_type'::regclass
                                                        AND d.objid = t.oid AND d.deptype = 'e')
  LOOP
    EXECUTE format('ALTER %s %s OWNER TO pacod_gw', CASE r.typtype WHEN 'd' THEN 'DOMAIN' ELSE 'TYPE' END, r.name);
  END LOOP;

  FOR r IN SELECT p.oid::regprocedure AS name, p.prokind FROM pg_proc p
            WHERE p.proowner = 10 AND p.oid >= 16384
              AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass
                                                        AND d.objid = p.oid AND d.deptype = 'e')
  LOOP
    EXECUTE format('ALTER %s %s OWNER TO pacod_gw',
                   CASE r.prokind WHEN 'a' THEN 'AGGREGATE' WHEN 'p' THEN 'PROCEDURE' ELSE 'FUNCTION' END, r.name);
  END LOOP;

  -- Whatever the superuser still owns here, of any kind, stops the whole change.
  SELECT string_agg(what, ', ') INTO left_over FROM (
    SELECT format('schema %I', nspname) AS what FROM pg_namespace n
     WHERE nspowner = 10 AND nspname NOT LIKE 'pg\_%' AND nspname <> 'information_schema'
       AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_namespace'::regclass
                                                 AND d.objid = n.oid AND d.deptype = 'e')
    UNION ALL
    SELECT format('relation %s', c.oid::regclass) FROM pg_class c
     WHERE relowner = 10 AND c.oid >= 16384
       AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_class'::regclass
                                                 AND d.objid = c.oid AND d.deptype = 'e')
    UNION ALL
    SELECT format('type %s', t.oid::regtype) FROM pg_type t
     WHERE typowner = 10 AND t.oid >= 16384
       AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_type'::regclass
                                                 AND d.objid = t.oid AND d.deptype = 'e')
    UNION ALL
    SELECT format('routine %s', p.oid::regprocedure) FROM pg_proc p
     WHERE proowner = 10 AND p.oid >= 16384
       AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass
                                                 AND d.objid = p.oid AND d.deptype = 'e')
    UNION ALL SELECT format('collation %s', oid) FROM pg_collation WHERE collowner = 10 AND oid >= 16384
    UNION ALL SELECT format('conversion %s', oid) FROM pg_conversion WHERE conowner = 10 AND oid >= 16384
    UNION ALL SELECT format('operator %s', oid) FROM pg_operator WHERE oprowner = 10 AND oid >= 16384
    UNION ALL SELECT format('operator class %s', oid) FROM pg_opclass WHERE opcowner = 10 AND oid >= 16384
    UNION ALL SELECT format('operator family %s', oid) FROM pg_opfamily WHERE opfowner = 10 AND oid >= 16384
    UNION ALL SELECT format('text search configuration %s', oid) FROM pg_ts_config WHERE cfgowner = 10 AND oid >= 16384
    UNION ALL SELECT format('text search dictionary %s', oid) FROM pg_ts_dict WHERE dictowner = 10 AND oid >= 16384
    UNION ALL SELECT format('statistics %s', oid) FROM pg_statistic_ext WHERE stxowner = 10
    UNION ALL SELECT format('event trigger %s', evtname) FROM pg_event_trigger WHERE evtowner = 10
    UNION ALL SELECT format('large object %s', oid) FROM pg_largeobject_metadata WHERE lomowner = 10
  ) owned;
  IF left_over IS NOT NULL THEN
    RAISE EXCEPTION 'the superuser owns objects in pacod_gw that this script does not move: %', left_over;
  END IF;
END
$$;
COMMIT;
SQL

# A session that logged in as oid 10 between step 4 and the change would keep its rights.
ended="$(pg_as postgres postgres <<'SQL'
SELECT count(*) FROM (
  SELECT pg_terminate_backend(pid) FROM pg_stat_activity
   WHERE usesysid = 10 AND backend_type = 'client backend' AND pid <> pg_backend_pid()
) ended;
SQL
)"
[ "$ended" = "0" ] || warn "ended $ended session(s) that had logged in as the superuser meanwhile"
pg_as postgres postgres <<<'DROP ROLE pgw_demote' >/dev/null

# ---------------------------------------------------------------------------------------------
# 6. Start, and check
# ---------------------------------------------------------------------------------------------
if [ "$start_stack" = yes ]; then
    info "Starting the stack (pacod-db-init gives pacod_gw the password GW_DB_PASSWORD)"
    compose up -d --wait
fi

[ "$(pg_as postgres postgres <<<"SELECT rolsuper FROM pg_roles WHERE rolname = 'pacod_gw'")" = "f" ] ||
    die "pacod_gw is still a superuser"
[ "$(pg_as postgres pacod_gw <<<"SELECT count(*) FROM pg_class WHERE relowner <> 'pacod_gw'::regrole AND oid >= 16384 AND relnamespace = 'public'::regnamespace")" = "0" ] ||
    die "some relations in pacod_gw are not owned by pacod_gw"
if [ "$(pg_as postgres postgres <<<"SELECT count(*) FROM pg_database WHERE datname = 'pacod_id'")" = "1" ]; then
    if denied="$(pg_as pacod_gw pacod_id <<<'SELECT 1' 2>&1)"; then
        die "pacod_gw can still connect to pacod-id's database"
    fi
    case "$denied" in
        *"permission denied"*) info "pacod_gw is refused by pacod-id's database, as it should be." ;;
        *) warn "could not confirm that pacod-id's database refuses pacod_gw: $denied" ;;
    esac
fi

info "Done: PostgreSQL's superuser is \"postgres\" with POSTGRES_PASSWORD; pacod_gw owns its database and nothing more."
[ "$start_stack" = yes ] || info "The gateways are stopped (--no-start). Start the stack with: docker compose ... up -d --wait"
info "Start the gateways on other machines again; their DATABASE_URL keeps working when GW_DB_PASSWORD is the old POSTGRES_PASSWORD."
