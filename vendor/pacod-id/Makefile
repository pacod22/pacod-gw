.PHONY: network build test test-rust test-node test-python fmt lint verify test-services deny e2e \
	image image-mock compose-up compose-down measure chart-check conformance

# The tests' PostgreSQL (scripts/test-services.sh). 127.0.0.1, not localhost: localhost may
# resolve to ::1, where nothing listens.
PID_TEST_DATABASE_URL ?= postgres://postgres:test@127.0.0.1:25434/pid
export PID_TEST_DATABASE_URL

build:
	cargo build --workspace
	npm run build

test: test-rust test-node test-python

test-rust:
	cargo test --workspace

test-node:
	npm test

test-python:
	cd python && uv run pytest -q

fmt:
	cargo fmt --all
	npx prettier --write .
	cd python && uv run ruff format .

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	npm run lint
	npm run check
	cd python && uv run ruff check . && uv run ruff format --check .

verify: lint test

test-services:
	scripts/test-services.sh

# The hosted pages in a real browser: three realms, mail, the mock Google (needs Docker).
e2e:
	scripts/e2e.sh

deny:
	cargo deny --locked check

# Images (deploy/docker/pacod-id.Dockerfile).
VERSION ?= dev
image:
	docker build -f deploy/docker/pacod-id.Dockerfile --build-arg VERSION=$(VERSION) \
		--build-arg VCS_REF=$$(git rev-parse --short HEAD) -t pacod-id:$(VERSION) .

image-mock:
	docker build -f deploy/docker/pacod-id.Dockerfile --target mock-idp -t pacod-id-mock-idp:$(VERSION) .

# The development stack (deploy/compose): pacod-id on http://127.0.0.1:26070, mail on :26026.
# The app file plus its standalone copies of the shared components, on the shared network
# pacod-net (pacod-demo/docs/NETWORK.md).
COMPOSE := docker compose -f deploy/compose/docker-compose.yml \
	-f deploy/compose/docker-compose.standalone.yml --env-file deploy/compose/.env

# The shared networks every pacod stack joins; created once, kept by "down". pacod-admin-net
# carries the listeners only servers reach (pacod-id's internal one) and must be internal, with
# no route out; compose never checks that, so this does.
network:
	@docker network inspect pacod-net >/dev/null 2>&1 || docker network create pacod-net >/dev/null
	@docker network inspect pacod-admin-net >/dev/null 2>&1 || docker network create --internal pacod-admin-net >/dev/null
	@test "$$(docker network inspect -f '{{.Internal}}' pacod-admin-net)" = true || \
		{ echo "pacod-admin-net is not internal: remove it (docker network rm pacod-admin-net) and run make network" >&2; exit 1; }

compose-up: network
	deploy/compose/init-env.sh
	$(COMPOSE) up -d --build --wait

compose-down:
	$(COMPOSE) down

# Latency of the token endpoint and of the feed, against the development stack.
measure:
	node scripts/measure.mjs

# The Helm chart: lint, then the rendered manifests against the Kubernetes schemas, with secrets
# as files, as variables and from an existing Secret. Needs Docker only.
HELM_IMAGE := alpine/helm:4.3.0
KUBECONFORM_IMAGE := ghcr.io/yannh/kubeconform:v0.8.0
chart-check:
	docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) lint pacod-id -f pacod-id/ci/test-values.yaml --strict
	for set in secrets.mount=true secrets.mount=false secrets.existingSecret=pid-secrets; do \
		docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) template pacod-id pacod-id \
			--namespace pacod-id -f pacod-id/ci/test-values.yaml --set "$$set" \
		| docker run --rm -i $(KUBECONFORM_IMAGE) -strict -summary -schema-location default \
			-schema-location 'https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json' - \
		|| exit 1; \
	done
	@# The chart's TOML through pacod-id's own check, when the image is here (make image).
	if docker image inspect pacod-id:$(VERSION) >/dev/null 2>&1; then \
		mkdir -p target/chart-check && \
		docker run --rm -v "$(CURDIR)/deploy/helm:/charts:ro" -w /charts $(HELM_IMAGE) template pacod-id pacod-id \
			-f pacod-id/ci/test-values.yaml --show-only templates/configmap.yaml \
			| sed -n '/pacod-id.toml: |/,$$p' | tail -n +2 | sed 's/^    //' > target/chart-check/pacod-id.toml && \
		docker run --rm -v "$(CURDIR)/target/chart-check/pacod-id.toml:/etc/pacod-id/pacod-id.toml:ro" \
			pacod-id:$(VERSION) check-config; \
	else echo "pacod-id:$(VERSION) not built: the chart's configuration is not checked (make image)"; fi

# The OpenID Foundation's conformance suite against the image (conformance/run.sh; needs
# OIDF_SUITE_DIR, a checkout of the suite at release-v5.3.2, and Docker).
conformance:
	conformance/run.sh
