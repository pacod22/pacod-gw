.PHONY: build check test fmt lint run-core run-analytics ui-dev ui-build up down \
	dev-up dev-down images deploy-check openapi openapi-check helm-rules backup \
	verify test-services smoke-mock smoke-live load

# Tag of the images built by "make images"; the same value as PGW_VERSION in deploy/.env.
VERSION ?= 0.1.0

COMPOSE := docker compose -f deploy/docker-compose.yml
COMPOSE_DEV := $(COMPOSE) -f deploy/docker-compose.dev.yml

# Tools for "make deploy-check", run from the images that deploy/ and CI pin.
CADDY_IMAGE := caddy:2.11.6-alpine
PROMETHEUS_IMAGE := prom/prometheus:v3.15.0
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

fmt:
	cargo fmt --all
	cd ui && npm run format

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cd ui && npm run lint

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
dev-up:
	$(COMPOSE_DEV) up -d --build --wait

dev-down:
	$(COMPOSE_DEV) down

# The three images of a release, tagged $(VERSION).
images:
	docker build -f deploy/docker/rust.Dockerfile --target core -t pacod-gw-core:$(VERSION) .
	docker build -f deploy/docker/rust.Dockerfile --target analytics -t pacod-gw-analytics:$(VERSION) .
	docker build -f deploy/docker/ui.Dockerfile -t pacod-gw-ui:$(VERSION) .

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
	@echo "== OpenTelemetry collector configuration"
	docker run --rm -v "$(CURDIR)/deploy/otel-collector/config.yaml:/etc/otelcol/config.yaml:ro" $(OTELCOL_IMAGE) validate --config=/etc/otelcol/config.yaml
	@echo "== shell scripts"
	docker run --rm -v "$(CURDIR):/mnt:ro" -w /mnt $(SHELLCHECK_IMAGE) -x deploy/scripts/backup.sh deploy/scripts/restore.sh deploy/scripts/lib.sh deploy/scripts/preflight.sh
	@echo "== Helm chart"
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) lint pacod-gw -f pacod-gw/ci/test-values.yaml --strict
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) template pacod-gw pacod-gw --namespace pacod-gw -f pacod-gw/ci/test-values.yaml \
		| docker run --rm -i $(KUBECONFORM_IMAGE) -strict -summary -schema-location default \
			-schema-location 'https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json' -
	cmp deploy/prometheus/rules/pacod-gw.rules.yml $(CHART)/files/pacod-gw.rules.yml
	@echo "== workflows"
	docker run --rm -v "$(CURDIR):/repo:ro" -w /repo $(ACTIONLINT_IMAGE)
	@echo "deploy-check: all checks passed"
