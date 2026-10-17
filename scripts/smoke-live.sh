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
export GEMINI_API_KEY="${GEMINI_API_KEY:-$(read_key GEMINI_API_KEY)}"
export DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY:-$(read_key DEEPSEEK_API_KEY)}"
export ZAI_API_KEY="${ZAI_API_KEY:-$(read_key ZAI_API_KEY)}"
# TypeSafe: one Jev classification through the intent plugin's try endpoint.
export TYPESAFE_API_KEY="${TYPESAFE_API_KEY:-$(read_key TYPESAFE_API_KEY)}"
# Vertex AI: the project, its location and the path of a service-account key file.
export VERTEX_PROJECT="${VERTEX_PROJECT:-$(read_key VERTEX_PROJECT)}"
export VERTEX_LOCATION="${VERTEX_LOCATION:-$(read_key VERTEX_LOCATION)}"
export VERTEX_CREDENTIALS_FILE="${VERTEX_CREDENTIALS_FILE:-$(read_key VERTEX_CREDENTIALS_FILE)}"

cargo run -q -p pacod-gw-testkit --bin gw-smoke -- --events live

logs="$(mktemp)"
trap 'rm -f "$logs"' EXIT
# Every container of the development stack, whichever layout runs (dev-up, dev-scale-up),
# of a gateway that plays another machine (node-dev-up), and of the minimal gateway.
for project in pacod-gw pacod-gw-node-dev pacod-gw-lite pacod-gw-lite-dev; do
  for container in $(docker ps -aq --filter "label=com.docker.compose.project=$project" 2>/dev/null); do
    docker logs "$container" >>"$logs" 2>&1 || true
  done
done
if [[ -s "$logs" ]]; then
  cargo run -q -p pacod-gw-testkit --bin gw-smoke -- scan-logs "$logs"
else
  echo "skip  the development stack is not running here; its logs were not scanned"
fi
