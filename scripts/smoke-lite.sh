#!/usr/bin/env bash
# The smoke test of the minimal gateway's development stack (make lite-dev-up): gw-smoke lite
# calls it, then every call it made must have its line in the gateway's log, the audited call
# its body line, the chat call its intent, and the log must hold no key. Arguments go to
# gw-smoke lite. The checks read the text format, the development stack's default: start it
# without PGW_LOG_FORMAT=json in the shell or in deploy/lite/.env.
set -euo pipefail
cd "$(dirname "$0")/.."

project=pacod-gw-lite-dev
compose=(docker compose -p "$project" -f deploy/lite/docker-compose.yml -f deploy/lite/docker-compose.dev.yml)

# The calls and the log checks both need the stack. Without it every call would fail on its
# own; one line says why instead. The full development stack (make dev-up) uses the same
# container names, so the two never run together.
if [[ -z "$("${compose[@]}" ps -q --status running pacod-gw 2>/dev/null)" ]]; then
  echo "FAIL  the minimal gateway's development stack is not running: make lite-dev-up"
  other="$(docker inspect -f '{{index .Config.Labels "com.docker.compose.project"}}' pacod-gw 2>/dev/null || true)"
  if [[ -n "$other" && "$other" != "$project" ]]; then
    echo "      the container pacod-gw belongs to the compose project $other, which uses the same names:"
    echo "      stop it first (make dev-down for the full development stack)"
  fi
  exit 1
fi

report="$(mktemp)"
logs="$(mktemp)"
trap 'rm -f "$report" "$logs"' EXIT

status=0
cargo run -q -p pacod-gw-testkit --bin gw-smoke -- lite "$@" | tee "$report" || status=1

# The log is written when a call settles, at the latest when its body has ended.
sleep 1
"${compose[@]}" logs --no-log-prefix pacod-gw >"$logs" 2>&1

ids=0
missing=0
while read -r _ id name; do
  ids=$((ids + 1))
  if ! grep -q "pacod_gw_lite::call: call request_id=$id " "$logs"; then
    echo "FAIL  no call line for $name ($id)"
    missing=$((missing + 1))
  fi
  if [[ "$name" == "call with x-gw-audit" ]] && ! grep -q "pacod_gw_lite::body: body request_id=$id " "$logs"; then
    echo "FAIL  no body line for $name ($id)"
    missing=$((missing + 1))
  fi
done < <(grep '^request-id ' "$report")
if [[ "$missing" -eq 0 ]]; then
  echo "ok    every one of $ids calls has its line in the gateway's log"
else
  status=1
fi
if grep -q "pacod_gw_lite::request: rejected method=POST path=/v1/chat/completions .* http=401" "$logs"; then
  echo "ok    the bad key has its rejected line"
else
  echo "FAIL  no rejected line for the bad key"
  status=1
fi

# The intent plugin, in shadow mode in the development stack: the first chat call's line
# carries its intent and verdict, and its classification has a line of its own naming it.
chat_id="$(grep '^request-id .* chat (OpenAI API)$' "$report" | head -n 1 | cut -d ' ' -f 2 || true)"
if [[ -n "$chat_id" ]] &&
  grep -qE "pacod_gw_lite::call: call request_id=$chat_id .* intent=[a-z_]+ .*verdict=[a-z_]+" "$logs" &&
  grep -q "task=plugin.intent parent_id=$chat_id" "$logs"; then
  echo "ok    the chat call has its intent, and its classification a line of its own"
else
  echo "FAIL  no intent on the chat call's line ($chat_id), or no classification line naming it"
  status=1
fi

# The development configuration's provider keys and the secret parts of its client keys.
leaks="$(grep -cE 'dev-(openai|anthropic|deepseek|classifier|typesafe|cohere)-key|DevelopmentOnlyKey|NotAKeyOfThisGateway' "$logs" || true)"
if [[ "$leaks" -eq 0 ]]; then
  echo "ok    no key in $(wc -l <"$logs" | tr -d ' ') log lines"
else
  echo "FAIL  $leaks log lines contain a key"
  status=1
fi
cargo run -q -p pacod-gw-testkit --bin gw-smoke -- scan-logs "$logs" || status=1
exit "$status"
