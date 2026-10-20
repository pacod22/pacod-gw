.PHONY: network build check test fmt lint run-core run-analytics ui-dev ui-build up down \
	dev-up dev-down images deploy-check openapi openapi-check helm-deps helm-rules backup \
	verify test-services smoke-mock smoke-live record-fixtures load tester-test run-tester \
	scale-up scale-down dev-scale-up dev-scale-down node-dev-up node-dev-down \
	hub-up hub-down node-up node-down deny \
	authz authz-check vendor-pacod-id \
	run-lite lite-up lite-down lite-check lite-dev-up lite-dev-down lite-smoke lite-live \
	load-agent-up load-agent-down load-agent-dev-up load-agent-dev-down

# Tag of the images built by "make images"; the same value as PGW_VERSION in deploy/.env.
VERSION ?= 0.1.0

# The gateway's own stack: the app file (the gateway's services) and the standalone file (its
# copies of the shared components), on the shared network pacod-net
# (pacod-demo/docs/NETWORK.md). The full stack loads the app file alone.
APP := -f deploy/docker-compose.yml
STANDALONE := -f deploy/docker-compose.standalone.yml
COMPOSE := docker compose $(APP) $(STANDALONE)
# The development stack: each file with its development override. It always has a load agent
# and the load tests' mock (docs/testing.md, 9).
COMPOSE_DEV := docker compose $(APP) -f deploy/docker-compose.dev.yml $(STANDALONE) -f deploy/docker-compose.standalone.dev.yml \
	-f deploy/docker-compose.load.yml -f deploy/docker-compose.load.dev.yml
# "make up LOAD_TEST=1" (or scale-up, hub-up): load tests from the console, with an agent in the
# stack. Off by default: an agent makes load on request.
LOAD_TEST_FILE := $(if $(filter 1,$(LOAD_TEST)),-f deploy/docker-compose.load.yml)
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
# A load agent on another machine; its settings are in deploy/load-agent/.env.
LOAD_AGENT := docker compose -f deploy/load-agent/docker-compose.yml
# A load agent that plays another machine, next to the development stack: 18196.
LOAD_AGENT_DEV := docker compose -p pacod-gw-load-agent-dev -f deploy/load-agent/docker-compose.yml -f deploy/load-agent/docker-compose.dev.yml
# The minimal gateway (docs/lite.md): one container, configured by deploy/lite/.env. It is
# pacod-gw on pacod-net, so it does not run on the same host as the full stack.
LITE := docker compose -f deploy/lite/docker-compose.yml
# The same built from the working tree, against the mock provider: 18280, mock 19280.
LITE_DEV := docker compose -p pacod-gw-lite-dev -f deploy/lite/docker-compose.yml -f deploy/lite/docker-compose.dev.yml

# Tools for "make deploy-check", run from pinned images.
CADDY_IMAGE := caddy:2.11.6-alpine
PROMETHEUS_IMAGE := prom/prometheus:v3.15.0
ALERTMANAGER_IMAGE := prom/alertmanager:v0.34.1
OTELCOL_IMAGE := otel/opentelemetry-collector:0.162.0
HELM_IMAGE := alpine/helm:4.3.0
KUBECONFORM_IMAGE := ghcr.io/yannh/kubeconform:v0.8.0
SHELLCHECK_IMAGE := koalaman/shellcheck:v0.11.0
CHART := deploy/helm/pacod-gw

# The shared network every pacod stack joins; created once, kept by "down".
network:
	@docker network inspect pacod-net >/dev/null 2>&1 || docker network create pacod-net >/dev/null

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

# Every check before a commit, in one go: format, lints, all tests, the OpenAPI drift check, and the
# console's lint, type check, tests and build.
verify: test-services
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	$(MAKE) test
	$(MAKE) openapi-check
	$(MAKE) authz-check
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
# SMOKE_AUTH=oidc signs in through pacod-id (a stack started with PGW_AUTH=oidc), as the
# development client gw-smoke; gw-load takes the same setting.
smoke-mock:
	cargo run -q -p pacod-gw-testkit --bin gw-smoke -- --events mock

# A few tiny calls per provider with the real keys in .env. Costs a fraction of a cent.
smoke-live:
	scripts/smoke-live.sh

# Records provider answers as fixtures, calling the providers directly with the keys in .env
# (GEMINI_API_KEY, DEEPSEEK_API_KEY, ZAI_API_KEY; VERTEX_PROJECT, VERTEX_LOCATION and
# VERTEX_CREDENTIALS_FILE for Vertex AI). RECORD_ARGS narrows it, e.g. "--provider glm".
record-fixtures:
	scripts/record-fixtures.sh $(RECORD_ARGS)

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

# The minimal gateway on the host, configured by LITE_ENV (default deploy/lite/.env).
LITE_ENV ?= deploy/lite/.env
run-lite:
	cargo run -p pacod-gw-lite -- --env-file $(LITE_ENV)

ui-dev:
	cd ui && npm run dev

ui-build:
	cd ui && npm run build

# The production stack. Needs deploy/.env (see deploy/.env.example) and the images.
up: network
	$(COMPOSE) $(LOAD_TEST_FILE) up -d --wait

down:
	$(COMPOSE) $(LOAD_TEST_FILE) down

# The development stack: builds the images, adds the mock provider, needs no deploy/.env.
# DEV_HTTPS=1 adds ports 80 and 443.
dev-up: network
	$(COMPOSE_DEV) $(DEV_HTTPS_FILE) up -d --build --wait

dev-down:
	$(COMPOSE_DEV) down

# The production stack with two gateways and a separate management instance.
scale-up: network
	$(COMPOSE_SCALE) $(LOAD_TEST_FILE) up -d --wait

scale-down:
	$(COMPOSE_SCALE) $(LOAD_TEST_FILE) down

# The central host when gateways on other machines join (docs/runbook.md, section 9).
hub-up: network
	$(COMPOSE_HUB) $(LOAD_TEST_FILE) up -d --wait

hub-down:
	$(COMPOSE_HUB) $(LOAD_TEST_FILE) down

# On another machine: one gateway that joins the central host. Needs deploy/node/.env.
node-up:
	$(NODE) up -d --wait

node-down:
	$(NODE) down

# The same for development: data plane through Caddy on 18080, the gateways on 18082 and
# 18083, the management API on 18081. DEV_HTTPS=1 adds ports 80 and 443.
dev-scale-up: network
	$(COMPOSE_DEV_SCALE) $(DEV_HTTPS_FILE) up -d --build --wait

dev-scale-down:
	$(COMPOSE_DEV_SCALE) down

# A gateway that plays another machine, next to the development stack (either layout). It
# reaches the stack through the published ports only; data plane 18180, metrics 19180.
node-dev-up:
	$(NODE_DEV) up -d --build --wait

node-dev-down:
	$(NODE_DEV) down

# On another machine: a load agent for the central tester. Needs deploy/load-agent/.env.
load-agent-up:
	$(LOAD_AGENT) up -d --wait

load-agent-down:
	$(LOAD_AGENT) down

# A load agent that plays another machine, next to the development stack; the development
# tester lists it as "remote" (host.docker.internal:18196).
load-agent-dev-up:
	$(LOAD_AGENT_DEV) up -d --build --wait

load-agent-dev-down:
	$(LOAD_AGENT_DEV) down

# The minimal gateway. Needs deploy/lite/.env and the image pacod-gw-lite.
lite-up: network
	$(LITE) up -d --wait

lite-down:
	$(LITE) down

# What the gateway reads from deploy/lite/.env, without secrets; fails when it would not start.
lite-check: network
	$(LITE) run --rm --no-deps pacod-gw check

# The minimal gateway for development: builds the images, needs no deploy/lite/.env. Not while
# the development stack of the full gateway runs: both have pacod-gw and gw-mock.
lite-dev-up: network
	$(LITE_DEV) up -d --build --wait

lite-dev-down:
	$(LITE_DEV) down

# The smoke test of the minimal gateway's development stack: gw-smoke lite, then each call's
# line in the gateway's log and no key in it.
lite-smoke:
	scripts/smoke-lite.sh

# The minimal gateway on the host against OpenAI and Anthropic, with the keys in .env. Costs a
# fraction of a cent.
lite-live:
	scripts/lite-live.sh

# The images of a release, tagged $(VERSION): the gateway's seven (the load agent and the mock
# serve load tests only) and pacod-id, built from its vendored copy.
images:
	docker build -f deploy/docker/rust.Dockerfile --target core -t pacod-gw-core:$(VERSION) .
	docker build -f deploy/docker/rust.Dockerfile --target analytics -t pacod-gw-analytics:$(VERSION) .
	docker build -f deploy/docker/rust.Dockerfile --target lite -t pacod-gw-lite:$(VERSION) .
	docker build -f deploy/docker/rust.Dockerfile --target load-agent -t pacod-gw-load-agent:$(VERSION) .
	docker build -f deploy/docker/rust.Dockerfile --target mock -t pacod-gw-mock:$(VERSION) .
	docker build -f deploy/docker/ui.Dockerfile -t pacod-gw-ui:$(VERSION) .
	docker build -f deploy/docker/tester.Dockerfile -t pacod-gw-tester:$(VERSION) .
	docker build -f vendor/pacod-id/deploy/docker/pacod-id.Dockerfile --target pacod-id -t pacod-id:$(VERSION) vendor/pacod-id

# Licences, banned crates, sources and advisories of the Rust dependencies (deny.toml).
# Needs cargo-deny: cargo install cargo-deny --locked
deny:
	cargo deny --locked check

# The console's typed module of the gateway's rules (ui/src/lib/authz.generated.ts), from
# authz/gw.yaml through pacod-id's generator. "authz-check" fails when it is out of date.
authz:
	cd ui && npm run authz:gen

authz-check:
	@tmp="$$(mktemp -d)" && trap 'rm -rf "$$tmp"' EXIT && \
	(cd ui && node_modules/.bin/pacod-id-authz gen ../authz/gw.yaml --out "$$tmp/authz.generated.ts" && \
	node_modules/.bin/prettier --stdin-filepath src/lib/authz.generated.ts < "$$tmp/authz.generated.ts" > "$$tmp/formatted.ts") && \
	diff -u ui/src/lib/authz.generated.ts "$$tmp/formatted.ts" && \
	echo "the console's rules module is up to date"

# Copies a pinned pacod-id into vendor/pacod-id and its npm packages into ui/vendor
# (PACOD_ID=<checkout>, REV=<commit>; defaults ../pacod-id and its HEAD).
vendor-pacod-id:
	scripts/vendor-pacod-id.sh $(PACOD_ID) $(REV)

# Fails when the committed OpenAPI document or the client generated from it no longer match
# what the core produces. Needs "npm ci" in ui/.
openapi-check:
	@tmp="$$(mktemp -d)" && trap 'rm -rf "$$tmp"' EXIT && \
	cargo run -q -p pacod-gw-core -- openapi > "$$tmp/openapi.json" && \
	diff -u ui/src/lib/api/openapi.json "$$tmp/openapi.json" && \
	(cd ui && node_modules/.bin/openapi-typescript src/lib/api/openapi.json -o "$$tmp/schema.d.ts" > /dev/null) && \
	diff -u ui/src/lib/api/schema.d.ts "$$tmp/schema.d.ts" && \
	echo "the OpenAPI document and the generated client are up to date"

# Packages the vendored pacod-id chart into the gateway's chart (charts/), after vendoring.
helm-deps:
	docker run --rm -v "$(CURDIR):/repo" -w /repo/deploy/helm $(HELM_IMAGE) dependency update pacod-gw

# The chart carries a copy of the alert rules; deploy-check fails when the two differ.
helm-rules:
	cp deploy/prometheus/rules/pacod-gw.rules.yml $(CHART)/files/pacod-gw.rules.yml

backup:
	deploy/scripts/backup.sh

# Static checks of everything in deploy/. Starts no service. Needs Docker and
# python3; every other tool runs from its pinned image.
deploy-check:
	@echo "== compose files"
	$(COMPOSE) config --quiet
	$(COMPOSE_DEV) config --quiet
	docker compose --env-file deploy/.env.example $(APP) $(STANDALONE) config --quiet
	docker compose --env-file deploy/.env.example $(APP) $(STANDALONE) -f deploy/docker-compose.scale.yml config --quiet
	docker compose --env-file deploy/.env.example $(APP) $(STANDALONE) -f deploy/docker-compose.scale.yml -f deploy/docker-compose.hub.yml config --quiet
	docker compose --env-file deploy/.env.example $(APP) $(STANDALONE) -f deploy/docker-compose.load.yml config --quiet
	docker compose --env-file deploy/.env.example $(APP) $(STANDALONE) -f deploy/docker-compose.scale.yml -f deploy/docker-compose.load.yml config --quiet
	$(COMPOSE_DEV_SCALE) config --quiet
	$(COMPOSE_DEV) -f deploy/docker-compose.dev.https.yml config --quiet
	$(COMPOSE_DEV_SCALE) -f deploy/docker-compose.dev.https.yml config --quiet
	docker compose --env-file deploy/node/.env.example -f deploy/node/docker-compose.yml config --quiet
	$(NODE_DEV) config --quiet
	docker compose --env-file deploy/load-agent/.env.example -f deploy/load-agent/docker-compose.yml config --quiet
	$(LOAD_AGENT_DEV) config --quiet
	PGW_LITE_ENV_FILE=.env.example docker compose --env-file deploy/lite/.env.example -f deploy/lite/docker-compose.yml config --quiet
	$(LITE_DEV) config --quiet
	@echo "== Caddyfiles, with the gateway's sites (platform/caddy) as compose mounts them"
	docker run --rm -v "$(CURDIR)/deploy/caddy/Caddyfile:/etc/caddy/Caddyfile:ro" -v "$(CURDIR)/platform/caddy:/etc/caddy/sites:ro" \
		-e GW_API_SITE="api.localhost http://:8088" -e GW_CONSOLE_SITE=console.localhost -e "GW_SITE_TLS=tls internal" \
		$(CADDY_IMAGE) caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
	docker run --rm -v "$(CURDIR)/deploy/caddy/Caddyfile.dev:/etc/caddy/Caddyfile:ro" -v "$(CURDIR)/platform/caddy:/etc/caddy/sites:ro" \
		-e GW_API_SITE="http://:8088 api.localhost" -e GW_CONSOLE_SITE="http://:13000 console.localhost" \
		$(CADDY_IMAGE) caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
	@echo "== pacod-id configuration (with the image make dev-up builds; skipped without it)"
	@if docker image inspect pacod-id:dev >/dev/null 2>&1; then \
		docker run --rm -e PGW_ID_ISSUER=https://id.example.com -e PGW_CONSOLE_URL=https://console.example.com \
			-v "$(CURDIR)/deploy/pacod-id/pacod-id.toml:/etc/pacod-id/pacod-id.toml:ro" pacod-id:dev check-config && \
		docker run --rm -v "$(CURDIR)/deploy/pacod-id/pacod-id.dev.toml:/etc/pacod-id/pacod-id.toml:ro" pacod-id:dev check-config; \
	else echo "skipped: no pacod-id:dev image"; fi
	@echo "== Prometheus configuration, alert rules and their tests"
	docker run --rm -v "$(CURDIR)/deploy/prometheus/prometheus.yml:/etc/prometheus/prometheus.yml:ro" -v "$(CURDIR)/deploy/prometheus/rules:/etc/prometheus/rules:ro" \
		-v "$(CURDIR)/platform/prometheus:/etc/prometheus/scrape.d:ro" -v "$(CURDIR)/deploy/prometheus/targets:/etc/prometheus/gw-targets:ro" \
		-v "$(CURDIR)/deploy/prometheus/targets-scale:/etc/prometheus/gw-targets-scale:ro" --entrypoint promtool $(PROMETHEUS_IMAGE) check config /etc/prometheus/prometheus.yml
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
	docker run --rm -v "$(CURDIR):/mnt:ro" -w /mnt $(SHELLCHECK_IMAGE) -x deploy/scripts/backup.sh deploy/scripts/restore.sh deploy/scripts/lib.sh deploy/scripts/preflight.sh deploy/postgres/id-db.sh platform/postgres/gw-db.sh scripts/smoke-live.sh scripts/smoke-lite.sh scripts/lite-live.sh scripts/record-fixtures.sh deploy/alertmanager/render.sh
	@echo "== Helm chart"
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) lint pacod-gw -f pacod-gw/ci/test-values.yaml --strict
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) template pacod-gw pacod-gw --namespace pacod-gw -f pacod-gw/ci/test-values.yaml \
		| docker run --rm -i $(KUBECONFORM_IMAGE) -strict -summary -schema-location default \
			-schema-location 'https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json' -
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) template pacod-gw pacod-gw --namespace pacod-gw -f pacod-gw/ci/test-values.yaml --set secrets.mount=true \
		| docker run --rm -i $(KUBECONFORM_IMAGE) -strict -summary -schema-location default \
			-schema-location 'https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json' -
	cmp deploy/prometheus/rules/pacod-gw.rules.yml $(CHART)/files/pacod-gw.rules.yml
	@echo "== the packaged pacod-id subchart is the vendored chart (after vendoring: helm dependency update)"
	@tmp="$$(mktemp -d)" && trap 'rm -rf "$$tmp"' EXIT && tar xzf $(CHART)/charts/pacod-id-0.1.0.tgz -C "$$tmp" && \
		diff -r --exclude=ci --exclude=Chart.yaml "$$tmp/pacod-id" vendor/pacod-id/deploy/helm/pacod-id
	@echo "deploy-check: all checks passed"
