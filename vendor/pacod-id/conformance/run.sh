#!/usr/bin/env bash
# The OpenID Foundation's conformance suite (release v5.3.2, its published images) against
# pacod-id (the image pacod-id:dev, `make image`). Results go to results/<plan>/.
#
#   OIDF_SUITE_DIR=~/src/conformance-suite conformance/run.sh [config] [basic] [logout]
#
# OIDF_SUITE_DIR is a checkout of https://gitlab.com/openid/conformance-suite at
# release-v5.3.2: its scripts/run-test-plan.py creates the plans and collects the results.
# drive.mjs plays the person in the browser (PID_E2E_CHROMIUM may name a Chromium).
# Ports 27443 (the suite), 27470 and 27472 (pacod-id), on 127.0.0.1 only.
set -euo pipefail
cd "$(dirname "$0")"
: "${OIDF_SUITE_DIR:?set OIDF_SUITE_DIR to a checkout of the conformance suite at release-v5.3.2}"
plans=("$@")
[ ${#plans[@]} -gt 0 ] || plans=(config basic logout)

mkdir -p .work
docker run --rm --entrypoint cat registry.gitlab.com/openid/conformance-suite/nginx:release-v5.3.2 \
  /etc/nginx/nginx.conf | sed 's/8443/27443/g' > .work/nginx.conf

compose() { docker compose -f docker-compose.yml "$@"; }
compose up -d
cleanup() {
  [ -n "${driver:-}" ] && kill "$driver" 2>/dev/null || true
  compose logs pacod-id > .work/pacod-id.log 2>&1 || true
  # What became of the back-channel logouts pacod-id sent to the suite.
  compose exec -T postgres psql -qU postgres -d pacod_id -c \
    "SELECT client_id, attempts, last_error FROM backchannel_outbox ORDER BY created_at" \
    > .work/backchannel.txt 2>&1 || true
  compose down -v >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 120); do
  curl -skf https://127.0.0.1:27443/api/runner/available >/dev/null \
    && curl -sf http://127.0.0.1:27472/readyz >/dev/null && break
  sleep 2
done
curl -skf https://127.0.0.1:27443/api/runner/available >/dev/null || { echo "the suite did not start" >&2; exit 1; }

# The test account gets a name, so that the profile scope has a claim to return.
compose exec -T postgres psql -qU postgres -d pacod_id \
  -c "UPDATE accounts SET display_name = 'Conformance Tester' WHERE email = 'oidf@pid.oidf.test'"

node drive.mjs > .work/drive.log 2>&1 &
driver=$!

# The suite's API by address: its public name is not resolved everywhere (resolvers that refuse
# public names pointing at 127.0.0.1). The browser maps the name itself (drive.mjs).
export CONFORMANCE_SERVER=https://127.0.0.1:27443/
export CONFORMANCE_SERVER_MTLS=https://localhost.emobix.co.uk:8444/
export CONFORMANCE_DEV_MODE=1
config=plans/pacod-id.json
status=0
for p in "${plans[@]}"; do
  case "$p" in
    config) plan="oidcc-config-certification-test-plan" ;;
    basic) plan="oidcc-basic-certification-test-plan[server_metadata=discovery][client_registration=static_client]" ;;
    logout) plan="oidcc-rp-initiated-logout-certification-test-plan[response_type=code][client_registration=static_client]" ;;
    backchannel) plan="oidcc-backchannel-rp-initiated-logout-certification-test-plan[response_type=code][client_registration=static_client]" ;;
    *) echo "unknown plan $p" >&2; exit 2 ;;
  esac
  rm -rf "results/$p" && mkdir -p "results/$p"
  echo "== $plan"
  uv run --quiet --no-project --with httpx --with pyparsing python "$OIDF_SUITE_DIR/scripts/run-test-plan.py" \
    --export-dir "results/$p" --no-parallel "$plan" "$config" 2>&1 | tee "results/$p/summary.txt" || status=1
done
exit $status
