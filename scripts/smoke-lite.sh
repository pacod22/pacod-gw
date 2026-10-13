#!/usr/bin/env bash
# The smoke test of the minimal gateway's development stack (make lite-dev-up): gw-smoke lite
# calls it, then every call it made must have its line in the gateway's log, the audited call
# its body line, and the log must hold no key. Arguments go to gw-smoke lite.
set -euo pipefail
cd "$(dirname "$0")/.."

compose=(docker compose -p pacod-gw-lite-dev -f deploy/lite/docker-compose.yml -f deploy/lite/docker-compose.dev.yml)
report="$(mktemp)"
logs="$(mktemp)"
trap 'rm -f "$report" "$logs"' EXIT

status=0
cargo run -q -p pacod-gw-testkit --bin gw-smoke -- lite "$@" | tee "$report" || status=1

# The log is written when a call settles, at the latest when its body has ended.
sleep 1
"${compose[@]}" logs --no-log-prefix gateway >"$logs" 2>&1

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

# The development configuration's provider keys and the secret parts of its client keys.
leaks="$(grep -cE 'dev-(openai|anthropic|deepseek)-key|DevelopmentOnlyKey|NotAKeyOfThisGateway' "$logs" || true)"
if [[ "$leaks" -eq 0 ]]; then
  echo "ok    no key in $(wc -l <"$logs" | tr -d ' ') log lines"
else
  echo "FAIL  $leaks log lines contain a key"
  status=1
fi
cargo run -q -p pacod-gw-testkit --bin gw-smoke -- scan-logs "$logs" || status=1
exit "$status"
