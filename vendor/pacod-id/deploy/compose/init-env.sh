#!/bin/sh
# Writes deploy/compose/.env with random secrets for the development stack, once. An existing
# file is left alone. The values are never printed; read them from the file when needed.
set -eu
cd "$(dirname "$0")"

if [ -f .env ]; then
  echo "deploy/compose/.env exists; keeping it"
  exit 0
fi

secret() { openssl rand -base64 "$1" | tr -d '\n'; }
# Letters and digits only: these go into a database URL.
word() { openssl rand -hex "$1"; }

umask 077
cat > .env <<EOF
# Secrets of the development stack, made by init-env.sh. Not used anywhere else.
PID_DEV_PG_SUPERUSER_PASSWORD=$(word 16)
PID_DB_PASSWORD=$(word 16)
PID_KEK=$(secret 32)
PID_TOKEN_PEPPER=$(secret 32)
PID_DEV_APP_SECRET=$(word 24)
PID_DEV_SERVICE_SECRET=$(word 24)
PID_DEV_GOOGLE_SECRET=$(word 24)
PID_BOOTSTRAP_ADMIN_PASSWORD=$(word 12)
# Tag of the pacod-id image to build and run.
PID_VERSION=dev
EOF
echo "wrote deploy/compose/.env"
