#!/usr/bin/env bash
# The hosted pages end to end in a real browser: pacod-id with three realms (one per brand), a
# mail sink (mailpit), the mock Google, and Playwright. Everything listens on 127.0.0.1 only,
# ports 27325-27399 (fixtures 27391-27393); mail never leaves the machine.
#
#   scripts/e2e.sh                    every test
#   scripts/e2e.sh look.spec.ts       the screenshot tour only (e2e/screenshots)
#
# PID_E2E_CHROMIUM may name a Chromium on the machine; otherwise `npx playwright install
# chromium` provides one.
set -euo pipefail
cd "$(dirname "$0")/.."

scripts/test-services.sh >/dev/null
docker rm -f pid-e2e-mail pid-e2e-kit3d >/dev/null 2>&1 || true
docker run -d --name pid-e2e-mail -p 127.0.0.1:27325:1025 -p 127.0.0.1:27326:8025 axllent/mailpit:v1.27 >/dev/null
docker exec pid-test-pg psql -U postgres -qc "DROP DATABASE IF EXISTS pid_e2e WITH (FORCE)" -c "CREATE DATABASE pid_e2e"
rm -rf e2e/screenshots e2e/test-results

npm run build -w ui >/dev/null
npm run build -w packages/client -w packages/sveltekit >/dev/null
ID_ISSUER=http://id.gateway.e2e.test:27370 npm run build -w fixtures/kit2 -w fixtures/kit3 >/dev/null
# SvelteKit 3 fixes the origin when building: the container's copy, with its own address.
ID_ISSUER=http://id.gateway.e2e.test:27370 FIXTURE_ORIGIN=http://kit3d.e2e.test:27393 FIXTURE_OUT=build-container \
  npm run build -w fixtures/kit3 >/dev/null
cargo build -q -p pacod-id -p pacod-id-mock-idp

children=()
cleanup() {
  for p in "${children[@]}"; do kill "$p" 2>/dev/null || true; done
  docker rm -f pid-e2e-mail pid-e2e-kit3d >/dev/null 2>&1 || true
}
trap cleanup EXIT

# Test secrets only.
GOOGLE_GW=e2e-google-secret-of-the-gateway
GOOGLE_BAZAAR=e2e-google-secret-of-bazaar

node e2e/app.mjs &
children+=($!)

PID_MOCK_ISSUER=http://127.0.0.1:27380 PID_MOCK_LISTEN=127.0.0.1:27380 \
  PID_MOCK_CLIENTS="gw-google:$GOOGLE_GW,bazaar-google:$GOOGLE_BAZAAR" \
  target/debug/pacod-id-mock-idp >e2e/mock.log 2>&1 &
children+=($!)

env -i PATH="$PATH" HOME="$HOME" \
  PID_CONFIG=e2e/config.toml \
  PID_DATABASE_URL=postgres://postgres:test@127.0.0.1:25434/pid_e2e \
  PID_KEK=ZTJlLWtleS1lbmNyeXB0aW9uLWtleS0zMi1ieXRlcyE= \
  PID_TOKEN_PEPPER=e2e-token-pepper-sixteen-bytes-or-more \
  E2E_DEMO_SECRET=e2e-demo-client-secret-0123456789abcdef \
  E2E_GOOGLE_GATEWAY="$GOOGLE_GW" E2E_GOOGLE_BAZAAR="$GOOGLE_BAZAAR" \
  PID_PUBLIC_LISTEN=127.0.0.1:27370 PID_INTERNAL_LISTEN=127.0.0.1:27371 PID_METRICS_LISTEN=127.0.0.1:27372 \
  PID_UI_DIR=ui/build \
  PID_SMTP_HOST=127.0.0.1 PID_SMTP_PORT=27325 PID_SMTP_TLS=none PID_EMAIL_FROM_ADDRESS=id@e2e.test \
  PID_BOOTSTRAP_ADMIN_EMAIL=root@e2e.test PID_BOOTSTRAP_ADMIN_PASSWORD="the e2e bootstrap passphrase" \
  PID_BOOTSTRAP_ADMIN_ROLES=id.admin PID_BOOTSTRAP_ADMIN_REALM=gateway \
  PID_ARGON2_M_KIB=8192 PID_LOG_FORMAT=text \
  target/debug/pacod-id serve >e2e/server.log 2>&1 &
children+=($!)

for _ in $(seq 1 60); do
  curl -sf http://127.0.0.1:27372/readyz >/dev/null && break
  sleep 0.5
done
curl -sf http://127.0.0.1:27372/readyz >/dev/null || { echo "pacod-id did not start; see e2e/server.log" >&2; exit 1; }

# The fixture applications: Kit 2 with sessions in memory, Kit 3 with sealed cookies, and the
# Kit 3 build again in a container that reaches pacod-id through its internal address only.
env -i PATH="$PATH" HOST=127.0.0.1 PORT=27391 ORIGIN=http://kit2.e2e.test:27391 APP_ORIGIN=http://kit2.e2e.test:27391 \
  ID_ISSUER=http://id.gateway.e2e.test:27370 ID_INTERNAL_URL=http://127.0.0.1:27371 ID_CLIENT_ID=demo-kit2 \
  ID_CLIENT_SECRET=e2e-demo-client-secret-0123456789abcdef APP_SESSION_SECRET=e2e-fixture-session-secret-0123456789 \
  APP_STORE=memory REFRESH_BEFORE=299 node fixtures/kit2/build >e2e/kit2.log 2>&1 &
children+=($!)
env -i PATH="$PATH" HOST=127.0.0.1 PORT=27392 ORIGIN=http://kit3.e2e.test:27392 APP_ORIGIN=http://kit3.e2e.test:27392 \
  ID_ISSUER=http://id.gateway.e2e.test:27370 ID_INTERNAL_URL=http://127.0.0.1:27371 ID_CLIENT_ID=demo-kit3 \
  ID_CLIENT_SECRET=e2e-demo-client-secret-0123456789abcdef APP_SESSION_SECRET=e2e-fixture-session-secret-0123456789 \
  REFRESH_BEFORE=299 node fixtures/kit3/build >e2e/kit3.log 2>&1 &
children+=($!)
docker run -d --name pid-e2e-kit3d -p 127.0.0.1:27393:3000 \
  -v "$PWD/fixtures/kit3/build-container:/app/build:ro" -v "$PWD/fixtures/kit3/package.json:/app/package.json:ro" -w /app \
  -e PORT=3000 -e ORIGIN=http://kit3d.e2e.test:27393 -e APP_ORIGIN=http://kit3d.e2e.test:27393 \
  -e ID_ISSUER=http://id.gateway.e2e.test:27370 -e ID_INTERNAL_URL=http://host.docker.internal:27371 \
  -e ID_CLIENT_ID=demo-kit3d -e ID_CLIENT_SECRET=e2e-demo-client-secret-0123456789abcdef \
  -e APP_SESSION_SECRET=e2e-fixture-session-secret-0123456789 -e REFRESH_BEFORE=299 \
  node:24-alpine node build >/dev/null
for port in 27391 27392 27393; do
  for _ in $(seq 1 40); do
    curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/" | grep -qE '^[23]' && break
    sleep 0.25
  done
done

npx playwright test -c e2e/playwright.config.ts "$@"
