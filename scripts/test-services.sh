#!/usr/bin/env bash
# Starts the PostgreSQL, ClickHouse, Valkey and headless browser the tests use, on ports that do
# not collide with other stacks on a development machine (25433, 28123, 26379 and 29222). Safe to
# run again: running containers are left alone. The passwords are test values and the ports are
# bound to 127.0.0.1.
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
# The response cache's store; nothing is written to disk.
start pgw-test-valkey -p 127.0.0.1:26379:6379 valkey/valkey:9.1.2-alpine3.24 \
  valkey-server --save "" --appendonly no --requirepass test
# The scraper's browser, as deploy/docker-compose.yml runs it. Its tests give each page a proxy in
# the test process, which the browser reaches as host.docker.internal.
start pgw-test-browser --shm-size=256m --user 65534:65534 -e HOME=/tmp --read-only --tmpfs /tmp \
  --cap-drop ALL --security-opt no-new-privileges --add-host host.docker.internal:host-gateway \
  -p 127.0.0.1:29222:9222 \
  chromedp/headless-shell:157.0.8103.27@sha256:7bee201478b2f6266aae01243740ee85c16281e32f42d3aed9fdf04f4eace7a1 \
  --disable-background-networking

for _ in $(seq 1 60); do
  if docker exec pgw-test-pg pg_isready -q -h 127.0.0.1 -U postgres 2>/dev/null &&
    curl -fsS -o /dev/null http://127.0.0.1:28123/ping 2>/dev/null &&
    docker exec pgw-test-valkey valkey-cli -a test --no-auth-warning ping 2>/dev/null | grep -qx PONG &&
    curl -fsS -o /dev/null -H 'Host: 127.0.0.1' http://127.0.0.1:29222/json/version 2>/dev/null; then
    echo "test services are ready: postgres on 25433, clickhouse on 28123, valkey on 26379, browser on 29222"
    exit 0
  fi
  sleep 1
done
echo "the test services did not become ready within 60 s" >&2
exit 1
