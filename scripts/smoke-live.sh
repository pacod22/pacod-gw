#!/usr/bin/env bash
# Runs the live smoke test: a few tiny calls per provider through a running gateway, with the
# provider keys from .env (or from the environment). Then checks that the logs of the
# development stack contain no key.
#
# The keys are exported to the smoke tool only. Nothing in this script prints them, and the
# tool redacts them from everything it reports.
set -euo pipefail
cd "$(dirname "$0")/.."

env_file="${ENV_FILE:-.env}"
read_key() {
  [[ -f "$env_file" ]] || return 0
  awk -F= -v k="$1" '$1==k{sub(/^[^=]*=/,""); print; exit}' "$env_file" | tr -d "\"'\r"
}
export OPENAI_API_KEY="${OPENAI_API_KEY:-$(read_key OPENAI_API_KEY)}"
export ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-$(read_key ANTHROPIC_API_KEY)}"

cargo run -q -p pacod-gw-testkit --bin gw-smoke -- --events live

logs="$(mktemp)"
trap 'rm -f "$logs"' EXIT
# Every container of the development stack, whichever layout runs (dev-up, dev-scale-up),
# and of a gateway that plays another machine (node-dev-up).
for project in pacod-gw pacod-gw-node-dev; do
  for container in $(docker ps -aq --filter "label=com.docker.compose.project=$project" 2>/dev/null); do
    docker logs "$container" >>"$logs" 2>&1 || true
  done
done
if [[ -s "$logs" ]]; then
  cargo run -q -p pacod-gw-testkit --bin gw-smoke -- scan-logs "$logs"
else
  echo "skip  the development stack is not running here; its logs were not scanned"
fi
