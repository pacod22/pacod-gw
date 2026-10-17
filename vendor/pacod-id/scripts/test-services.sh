#!/usr/bin/env bash
# Starts the PostgreSQL the tests use, on a port that does not collide with other stacks on a
# development machine (25434). Safe to run again: a running container is left alone. The
# password is a test value and the port is bound to 127.0.0.1.
set -euo pipefail

NAME=pid-test-pg

if ! docker ps --format '{{.Names}}' | grep -qx "$NAME"; then
  if docker ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
    docker start "$NAME" >/dev/null
  else
    # Parallel tests run many queries at once: Docker's 64 MB of shared memory runs out.
    docker run -d --name "$NAME" --shm-size=256m -e POSTGRES_PASSWORD=test -e POSTGRES_DB=pid \
      -p 127.0.0.1:25434:5432 postgres:18.6-alpine3.24 >/dev/null
  fi
fi

for _ in $(seq 1 60); do
  if docker exec "$NAME" pg_isready -q -h 127.0.0.1 -U postgres 2>/dev/null; then
    echo "test postgres is ready on 127.0.0.1:25434"
    exit 0
  fi
  sleep 1
done
echo "the test postgres did not become ready within 60 s" >&2
exit 1
