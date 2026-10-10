.PHONY: build check test fmt lint run-core run-analytics ui-dev ui-build up down \
	dev-up dev-down images deploy-check openapi openapi-check helm-rules backup \
	verify test-services smoke-mock smoke-live load tester-test run-tester \
	scale-up scale-down dev-scale-up dev-scale-down node-dev-up node-dev-down \
	hub-up hub-down node-up node-down

# Tag of the images built by "make images"; the same value as PGW_VERSION in deploy/.env.
VERSION ?= 0.1.0

COMPOSE := docker compose -f deploy/docker-compose.yml
COMPOSE_DEV := $(COMPOSE) -f deploy/docker-compose.dev.yml
# Several gateways on one host, the management API on its own (docs/runbook.md, section 9).
COMPOSE_SCALE := $(COMPOSE) -f deploy/docker-compose.scale.yml
COMPOSE_DEV_SCALE := $(COMPOSE_DEV) -f deploy/docker-compose.scale.yml -f deploy/docker-compose.scale.dev.yml
# "make dev-up DEV_HTTPS=1" (or dev-scale-up): Caddy also on ports 80 and 443, with
# https://api.localhost and https://console.localhost as in production. The file comes last.
DEV_HTTPS_FILE := $(if $(filter 1,$(DEV_HTTPS)),-f deploy/docker-compose.dev.https.yml)
# The central host for gateways on other machines: the scale layout, plus PostgreSQL, analytics
# and OTLP offered on PGW_CLUSTER_BIND.
COMPOSE_HUB := $(COMPOSE_SCALE) -f deploy/docker-compose.hub.yml
# A gateway on another machine; its settings are in deploy/node/.env.
NODE := docker compose -f deploy/node/docker-compose.yml
# A gateway that plays another machine: its own project, no network shared with the stack.
NODE_DEV := docker compose -p pacod-gw-node-dev -f deploy/node/docker-compose.yml -f deploy/node/docker-compose.dev.yml

# Tools for "make deploy-check", run from the images that deploy/ and CI pin.
CADDY_IMAGE := caddy:2.11.6-alpine
PROMETHEUS_IMAGE := prom/prometheus:v3.15.0
ALERTMANAGER_IMAGE := prom/alertmanager:v0.34.1
OTELCOL_IMAGE := otel/opentelemetry-collector:0.162.0
HELM_IMAGE := alpine/helm:4.3.0
KUBECONFORM_IMAGE := ghcr.io/yannh/kubeconform:v0.8.0
SHELLCHECK_IMAGE := koalaman/shellcheck:v0.11.0
ACTIONLINT_IMAGE := rhysd/actionlint:1.7.12
CHART := deploy/helm/pacod-gw

build:
	cargo build --workspace
	cd ui && npm run build

check:
	cargo check --workspace
	cd ui && npm run check

# PostgreSQL and ClickHouse for the tests, in containers on ports 25433 and 28123.
test-services:
	scripts/test-services.sh

# The Rust tests need "make test-services" (or GW_TEST_DATABASE_URL and
# GW_TEST_CLICKHOUSE_URL pointing at servers of your own).
test:
	GW_TEST_CLICKHOUSE_URL=$${GW_TEST_CLICKHOUSE_URL:-http://localhost:28123} cargo test --workspace

# Everything CI checks, in one go: format, lints, all tests, the OpenAPI drift check, and the
# console's lint, type check, tests and build.
verify: test-services
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	$(MAKE) test
	$(MAKE) openapi-check
	cd ui && npm run lint && npm run check && npm test && npm run build
	$(MAKE) tester-test
	@echo "verify: all checks passed"

# Writes the core's OpenAPI document and the typed client generated from it into ui/.
openapi:
	cargo run -q -p pacod-gw-core -- openapi > ui/src/lib/api/openapi.json
	cd ui && node_modules/.bin/openapi-typescript src/lib/api/openapi.json -o src/lib/api/schema.d.ts

# The smoke tests run against a gateway that is already up: the development stack by default
# ("make dev-up"), or any other through SMOKE_ADMIN_URL, SMOKE_DATA_URL and SMOKE_MOCK_URL;
# SMOKE_MOCK_CONTROL_URL is the mock as the smoke tool reaches it, to script a failover.
smoke-mock:
	cargo run -q -p pacod-gw-testkit --bin gw-smoke -- --events mock

# A few tiny calls per provider with the real keys in .env. Costs a fraction of a cent.
smoke-live:
	scripts/smoke-live.sh

# Load through the gateway against the mock provider, compared with the mock called directly.
# PROFILE=proxy or PROFILE=managed; LOAD_ARGS adds options, e.g. "--requests 20000 --concurrency 500".
PROFILE ?= proxy
load:
	cargo run -q --release -p pacod-gw-testkit --bin gw-load -- --profile $(PROFILE) $(LOAD_ARGS)

# The tester (tester/): lint, format check and tests. Needs uv.
tester-test:
	cd tester && uv run --frozen ruff check . && uv run --frozen ruff format --check . && uv run --frozen pytest -q

# The tester on the host, against "scripts/dev-core.sh" by default. Without a mock it offers live
# mode only; TESTER_MOCK_URL=http://... adds mock mode. The console finds it with
# TESTER_URL=http://127.0.0.1:28095.
run-tester:
	cd tester && PGW_CORE_ADMIN_URL=$${PGW_CORE_ADMIN_URL:-http://127.0.0.1:28081} \
		PGW_CORE_DATA_URL=$${PGW_CORE_DATA_URL:-http://127.0.0.1:28080} \
		TESTER_DATA_DIR=$${TESTER_DATA_DIR:-.data} TESTER_LISTEN=$${TESTER_LISTEN:-127.0.0.1:28095} \
		uv run --frozen python -m pgw_tester

fmt:
	cargo fmt --all
	cd ui && npm run format
	cd tester && uv run --frozen ruff format .

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cd ui && npm run lint
	cd tester && uv run --frozen ruff check .

run-core:
	cargo run -p pacod-gw-core

run-analytics:
	cargo run -p pacod-gw-analytics

ui-dev:
	cd ui && npm run dev

ui-build:
	cd ui && npm run build

# The production stack. Needs deploy/.env (see deploy/.env.example) and the images.
up:
	$(COMPOSE) up -d --wait

down:
	$(COMPOSE) down

# The development stack: builds the images, adds the mock provider, needs no deploy/.env.
# DEV_HTTPS=1 adds ports 80 and 443.
dev-up:
	$(COMPOSE_DEV) $(DEV_HTTPS_FILE) up -d --build --wait

dev-down:
	$(COMPOSE_DEV) down

# The production stack with two gateways and a separate management instance.
scale-up:
	$(COMPOSE_SCALE) up -d --wait

scale-down:
	$(COMPOSE_SCALE) down

# The central host when gateways on other machines join (docs/runbook.md, section 9).
hub-up:
	$(COMPOSE_HUB) up -d --wait

hub-down:
	$(COMPOSE_HUB) down

# On another machine: one gateway that joins the central host. Needs deploy/node/.env.
node-up:
	$(NODE) up -d --wait

node-down:
	$(NODE) down

# The same for development: data plane through Caddy on 18080, the gateways on 18082 and
# 18083, the management API on 18081. DEV_HTTPS=1 adds ports 80 and 443.
dev-scale-up:
	$(COMPOSE_DEV_SCALE) $(DEV_HTTPS_FILE) up -d --build --wait

dev-scale-down:
	$(COMPOSE_DEV_SCALE) down

# A gateway that plays another machine, next to the development stack (either layout). It
# reaches the stack through the published ports only; data plane 18180, metrics 19180.
node-dev-up:
	$(NODE_DEV) up -d --build --wait

node-dev-down:
	$(NODE_DEV) down

# The four images of a release, tagged $(VERSION).
images:
	docker build -f deploy/docker/rust.Dockerfile --target core -t pacod-gw-core:$(VERSION) .
	docker build -f deploy/docker/rust.Dockerfile --target analytics -t pacod-gw-analytics:$(VERSION) .
	docker build -f deploy/docker/ui.Dockerfile -t pacod-gw-ui:$(VERSION) .
	docker build -f deploy/docker/tester.Dockerfile -t pacod-gw-tester:$(VERSION) .

# Fails when the committed OpenAPI document or the client generated from it no longer match
# what the core produces. Needs "npm ci" in ui/.
openapi-check:
	@tmp="$$(mktemp -d)" && trap 'rm -rf "$$tmp"' EXIT && \
	cargo run -q -p pacod-gw-core -- openapi > "$$tmp/openapi.json" && \
	diff -u ui/src/lib/api/openapi.json "$$tmp/openapi.json" && \
	(cd ui && node_modules/.bin/openapi-typescript src/lib/api/openapi.json -o "$$tmp/schema.d.ts" > /dev/null) && \
	diff -u ui/src/lib/api/schema.d.ts "$$tmp/schema.d.ts" && \
	echo "the OpenAPI document and the generated client are up to date"

# The chart carries a copy of the alert rules; CI fails when the two differ.
helm-rules:
	cp deploy/prometheus/rules/pacod-gw.rules.yml $(CHART)/files/pacod-gw.rules.yml

backup:
	deploy/scripts/backup.sh

# Static checks of everything in deploy/ and .github/. Starts no service. Needs Docker and
# python3; every other tool runs from its pinned image.
deploy-check:
	@echo "== compose files"
	$(COMPOSE) config --quiet
	$(COMPOSE_DEV) config --quiet
	docker compose --env-file deploy/.env.example -f deploy/docker-compose.yml config --quiet
	docker compose --env-file deploy/.env.example -f deploy/docker-compose.yml -f deploy/docker-compose.scale.yml config --quiet
	docker compose --env-file deploy/.env.example -f deploy/docker-compose.yml -f deploy/docker-compose.scale.yml -f deploy/docker-compose.hub.yml config --quiet
	$(COMPOSE_DEV_SCALE) config --quiet
	$(COMPOSE_DEV) -f deploy/docker-compose.dev.https.yml config --quiet
	$(COMPOSE_DEV_SCALE) -f deploy/docker-compose.dev.https.yml config --quiet
	docker compose --env-file deploy/node/.env.example -f deploy/node/docker-compose.yml config --quiet
	$(NODE_DEV) config --quiet
	@echo "== Caddyfile"
	docker run --rm -v "$(CURDIR)/deploy/caddy:/etc/caddy:ro" $(CADDY_IMAGE) caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
	docker run --rm -v "$(CURDIR)/deploy/caddy:/etc/caddy:ro" $(CADDY_IMAGE) caddy validate --config /etc/caddy/Caddyfile.dev --adapter caddyfile
	@echo "== Prometheus configuration, alert rules and their tests"
	docker run --rm -v "$(CURDIR)/deploy/prometheus:/etc/prometheus:ro" --entrypoint promtool $(PROMETHEUS_IMAGE) check config /etc/prometheus/prometheus.yml
	docker run --rm -v "$(CURDIR)/deploy/prometheus:/etc/prometheus:ro" --entrypoint promtool $(PROMETHEUS_IMAGE) check rules /etc/prometheus/rules/pacod-gw.rules.yml
	docker run --rm -v "$(CURDIR)/deploy/prometheus:/etc/prometheus:ro" --entrypoint promtool $(PROMETHEUS_IMAGE) test rules /etc/prometheus/tests/pacod-gw.rules.test.yml
	@echo "== dashboards and rules use only metrics the core exports; dashboard PromQL parses"
	mkdir -p target/deploy-check
	python3 -B deploy/scripts/check-dashboards.py --emit-rules target/deploy-check/dashboard-expressions.yml
	docker run --rm -v "$(CURDIR)/target/deploy-check:/work:ro" --entrypoint promtool $(PROMETHEUS_IMAGE) check rules /work/dashboard-expressions.yml
	@echo "== Alertmanager configuration, as render.sh writes it with and without email"
	for env in "" "-e PGW_SMTP_HOST=smtp.example.com -e PGW_SMTP_USERNAME=u -e PGW_SMTP_PASSWORD=p -e PGW_ALERT_EMAIL_TO=ops@example.com -e PGW_ALERT_WEBHOOK_URL=https://hooks.example.com/x"; do \
		docker run --rm $$env -v "$(CURDIR)/deploy/alertmanager/render.sh:/render.sh:ro" --entrypoint /bin/sh $(ALERTMANAGER_IMAGE) \
			-c '/bin/sh /render.sh --render-only && amtool check-config /tmp/alertmanager.yml' || exit 1; \
	done
	@echo "== OpenTelemetry collector configuration"
	docker run --rm -v "$(CURDIR)/deploy/otel-collector/config.yaml:/etc/otelcol/config.yaml:ro" $(OTELCOL_IMAGE) validate --config=/etc/otelcol/config.yaml
	@echo "== shell scripts"
	docker run --rm -v "$(CURDIR):/mnt:ro" -w /mnt $(SHELLCHECK_IMAGE) -x deploy/scripts/backup.sh deploy/scripts/restore.sh deploy/scripts/lib.sh deploy/scripts/preflight.sh scripts/smoke-live.sh deploy/alertmanager/render.sh
	@echo "== Helm chart"
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) lint pacod-gw -f pacod-gw/ci/test-values.yaml --strict
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) template pacod-gw pacod-gw --namespace pacod-gw -f pacod-gw/ci/test-values.yaml \
		| docker run --rm -i $(KUBECONFORM_IMAGE) -strict -summary -schema-location default \
			-schema-location 'https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json' -
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) template pacod-gw pacod-gw --namespace pacod-gw -f pacod-gw/ci/test-values.yaml --set secrets.mount=true \
		| docker run --rm -i $(KUBECONFORM_IMAGE) -strict -summary -schema-location default \
			-schema-location 'https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json' -
	cmp deploy/prometheus/rules/pacod-gw.rules.yml $(CHART)/files/pacod-gw.rules.yml
	@echo "== workflows"
	docker run --rm -v "$(CURDIR):/repo:ro" -w /repo $(ACTIONLINT_IMAGE)
	@echo "deploy-check: all checks passed"
