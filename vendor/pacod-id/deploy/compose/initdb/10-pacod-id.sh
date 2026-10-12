#!/bin/sh
# The first start of the stack's PostgreSQL: the role pacod_id, not a superuser, owning the
# database pacod_id. pacod-id creates its tables there (migrations run at start).
set -eu

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname postgres -v pw="$PID_DB_PASSWORD" <<'SQL'
CREATE ROLE pacod_id LOGIN PASSWORD :'pw' NOSUPERUSER NOCREATEDB NOCREATEROLE;
CREATE DATABASE pacod_id OWNER pacod_id;
REVOKE ALL ON DATABASE pacod_id FROM PUBLIC;
SQL
