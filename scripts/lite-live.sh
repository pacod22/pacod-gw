#!/usr/bin/env bash
# The minimal gateway against the real providers: starts pacod-gw-lite on the host with the
# OpenAI and Anthropic keys from .env (or the environment), makes a few tiny calls through it
# with gw-smoke lite, and checks that each call's log line has a cost and that the log holds
# no key. Costs a fraction of a cent.
#
# The keys are exported to the gateway's environment only, never passed as arguments, and
# nothing here prints them. LITE_LIVE_OPENAI_MODEL and LITE_LIVE_ANTHROPIC_MODEL choose the
# models; the rates below are test values, not the providers' prices.
set -euo pipefail
cd "$(dirname "$0")/.."

env_file="${ENV_FILE:-.env}"
read_key() {
  [[ -f "$env_file" ]] || return 0
  awk -F= -v k="$1" '$1==k{sub(/^[^=]*=/,""); print; exit}' "$env_file" | tr -d "\"'\r"
}
openai_key="${OPENAI_API_KEY:-$(read_key OPENAI_API_KEY)}"
anthropic_key="${ANTHROPIC_API_KEY:-$(read_key ANTHROPIC_API_KEY)}"
if [[ -z "$openai_key" || -z "$anthropic_key" ]]; then
  echo "OPENAI_API_KEY and ANTHROPIC_API_KEY are needed, in $env_file or the environment" >&2
  exit 1
fi
openai_model="${LITE_LIVE_OPENAI_MODEL:-gpt-4.1-nano}"
anthropic_model="${LITE_LIVE_ANTHROPIC_MODEL:-claude-haiku-4-5-20251001}"
port="${LITE_LIVE_PORT:-27280}"

cargo build -q -p pacod-gw-lite -p pacod-gw-testkit --bin pacod-gw-lite --bin gw-smoke

log="$(mktemp)"
report="$(mktemp)"
client_key="pgw_live_$(openssl rand -hex 16)"
pid=""
trap 'if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; fi; rm -f "$log" "$report"' EXIT

(
  export PGW_LISTEN="127.0.0.1:$port"
  export PGW_LOG_BODIES=on_request
  # The checks below read the text lines.
  export PGW_LOG_FORMAT=text
  export PGW_PROVIDER_OPENAI_KEY="$openai_key"
  export PGW_PROVIDER_ANTHROPIC_KEY="$anthropic_key"
  export PGW_MODEL_OAI="openai/$openai_model"
  export PGW_MODEL_OAI_PRICE="input=1,output=1"
  export PGW_MODEL_CLAUDE="anthropic/$anthropic_model"
  export PGW_MODEL_CLAUDE_PRICE="input=1,output=1"
  export PGW_POOL_BOTH="openai/$openai_model,anthropic/$anthropic_model"
  export PGW_CLIENT_LIVE_KEY="$client_key"
  exec target/debug/pacod-gw-lite serve
) >"$log" 2>&1 &
pid=$!
for _ in $(seq 1 50); do
  curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break
  sleep 0.2
done

status=0
SMOKE_LITE_KEY="$client_key" target/debug/gw-smoke lite \
  --url "http://127.0.0.1:$port" --limited-key "" --mock-control-url "" \
  --openai-model "openai/$openai_model" --anthropic-model "anthropic/$anthropic_model" --pool both \
  --embedding-model "" --speech-model "" --proxy-provider openai --proxy-model "$openai_model" \
  | tee "$report" || status=1

sleep 1
kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null || true
pid=""

ids=0
costless=0
while read -r _ id name; do
  ids=$((ids + 1))
  line="$(grep "request_id=$id " "$log" | grep "pacod_gw_lite::call" || true)"
  if [[ "$line" != *"status=ok"* || ! "$line" =~ cost=[0-9.e-]+ || "$line" == *"cost=0 "* ]]; then
    echo "FAIL  $name ($id): no line with status=ok and a cost"
    costless=$((costless + 1))
  fi
done < <(grep '^request-id ' "$report")
if [[ "$costless" -eq 0 ]]; then
  echo "ok    $ids calls logged with status=ok and a cost"
else
  status=1
fi
OPENAI_API_KEY="$openai_key" ANTHROPIC_API_KEY="$anthropic_key" target/debug/gw-smoke scan-logs "$log" || status=1
if grep -q "${client_key#pgw_live_}" "$log"; then
  echo "FAIL  the client key's secret is in the log"
  status=1
fi
exit "$status"
