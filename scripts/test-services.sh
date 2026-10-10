#!/usr/bin/env bash
# Starts the PostgreSQL and ClickHouse the tests use, on ports that do not collide with other
# stacks on a development machine (25433 and 28123). Safe to run again: running containers are
# left alone. The password is a test value and the ports are bound to 127.0.0.1.
set -euo pipefail

start() {
  local name="$1"
  shift
  if docker ps --format '{{.Names}}' | grep -qx "$name"; then
    return
  fi
  if docker ps -a --format '{{.Names}}' | grep -qx "$name"; then
    docker start "$name" >/dev/null
  else
    docker run -d --name "$name" "$@" >/dev/null
  fi
}

# Parallel tests run many queries at once: Docker's 64 MB of shared memory runs out.
start pgw-test-pg --shm-size=256m -e POSTGRES_PASSWORD=test -e POSTGRES_DB=gw -p 127.0.0.1:25433:5432 \
  postgres:18.6-alpine3.24
start pgw-test-ch -e CLICKHOUSE_SKIP_USER_SETUP=1 --ulimit nofile=262144:262144 -p 127.0.0.1:28123:8123 \
  clickhouse/clickhouse-server:26.8.17.4

for _ in $(seq 1 60); do
  if docker exec pgw-test-pg pg_isready -q -h 127.0.0.1 -U postgres 2>/dev/null &&
    curl -fsS -o /dev/null http://127.0.0.1:28123/ping 2>/dev/null; then
    echo "test services are ready: postgres on 25433, clickhouse on 28123"
    exit 0
  fi
  sleep 1
done
echo "the test services did not become ready within 60 s" >&2
exit 1
