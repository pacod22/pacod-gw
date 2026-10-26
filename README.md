# pacod-gw

An AI gateway: a Rust core with a proxy mode and a managed mode, an analytics service on
ClickHouse, a SvelteKit console, and the deployment files to run them. The same core also
runs as a minimal gateway: one container without database or console, configured by one
`.env` file.

- [docs/ai-gateway-concept.md](docs/ai-gateway-concept.md): the design.
- [docs/architecture.md](docs/architecture.md): how the code is laid out, what a request goes through, and where the code differs from the design.
- [docs/runbook.md](docs/runbook.md): deployment and operations.
- [docs/lite.md](docs/lite.md): the minimal gateway, one container without database or console, configured by one `.env` file.
- [docs/testing.md](docs/testing.md): the tester, which runs test cases and load tests against a running installation from the console.
- [docs/routing.md](docs/routing.md): routing schemes, rules and algorithms.
- [docs/plugin-concept.md](docs/plugin-concept.md): plugins, the intent plugin with its LLM and Jev engines, and the response cache.
- [CLAUDE.md](CLAUDE.md): notes for working on the code (commands, rules, conventions).
- [docs/threat-model.md](docs/threat-model.md): what the gateway protects, how, and what risk remains. Report weaknesses as [SECURITY.md](SECURITY.md) says.

Licensed under the Apache License 2.0 ([LICENSE](LICENSE)).

## Layout

```text
pacod-gw/
├── crates/
│   ├── pacod-gw-core/         # the gateway: data plane, management API, configuration
│   ├── pacod-gw-dialects/     # the neutral model; OpenAI, Anthropic, Gemini and native translators
│   ├── pacod-gw-events/       # the contract between core and analytics
│   ├── pacod-gw-analytics/    # ingest, ClickHouse storage, reports, scheduled jobs
│   ├── pacod-gw-lite/         # the minimal gateway: the data plane alone, configured by .env
│   ├── pacod-gw-plugin/       # the contract between the core and its plugins
│   ├── pacod-gw-plugin-cache/   # the response cache plugin, installed by default and off until switched on
│   ├── pacod-gw-plugin-intent/  # the intent plugin: LLM, Jev and cascade engines
│   ├── pacod-gw-load/         # the load engine and the load agent behind load tests
│   └── pacod-gw-testkit/      # mock providers, smoke test, load generator (not shipped)
├── ui/                        # the console
├── tester/                    # the test engine behind the console's Testing page (Python)
├── authz/                     # roles and permissions, the source of the core's and the console's checks
├── vendor/pacod-id/           # the identity service (OpenID Connect), vendored at a pinned commit
├── deploy/                    # Dockerfiles, compose files, Caddy, Helm chart, Prometheus, Grafana;
│                              #   deploy/lite/ for the minimal gateway
├── platform/                  # the gateway's share of the shared configuration: Caddy sites,
│                              #   PostgreSQL role, Prometheus scrape jobs
├── scripts/                   # development helpers, smoke and live checks
├── docs/
└── Makefile
```

## Getting started

Prerequisites: Rust 1.99, Node 24, uv (for the tester) and Docker.

```sh
make test-services   # PostgreSQL, ClickHouse and Valkey for the tests (ports 25433, 28123, 26379)
make verify          # format, lints, all tests, OpenAPI drift check, console checks

make dev-up          # the whole stack in containers, with a mock provider
make smoke-mock      # an end-to-end flow against the mock
open http://localhost:13000   # the console: admin@example.com / pacod-gw-dev
```

`make dev-up DEV_HTTPS=1` also puts Caddy on ports 80 and 443, as in production:
`https://console.localhost` and `https://api.localhost`, with certificates from Caddy's own
authority (runbook, section 11).

`make smoke-live` makes a few tiny calls through the gateway to OpenAI, Anthropic, Gemini,
DeepSeek and GLM. It reads `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`,
`DEEPSEEK_API_KEY` and `ZAI_API_KEY` from `.env` (see `.env.example`), skips a provider whose
key is missing, and never prints them. `make record-fixtures` records what Gemini, DeepSeek
and GLM answer to a fixed set of requests (`crates/pacod-gw-testkit/fixtures/`).
Kimi and Ollama are checked against the mock's emulations only.

In the console, platform admins find **Platform → Testing**: a catalog of test cases run against
the gateway by the tester container, in mock mode (the development stack's mock provider) or
live mode (models that are set up). See [docs/testing.md](docs/testing.md).

For production, start from [deploy/.env.example](deploy/.env.example) and the runbook.

The minimal gateway is the other option: the same data plane in one container, without
database, console or management API, configured by `deploy/lite/.env` and logging every call.

```sh
make lite-dev-up     # built from the working tree, with the mock provider (port 18280)
make lite-smoke      # calls through it, then finds each call's line in its log
make lite-up         # production: deploy/lite/.env (see deploy/lite/.env.example), port 14080
```

See [docs/lite.md](docs/lite.md). It is `pacod-gw` on the shared network (below), so it does
not run on the same host as the full stack: `make dev-down` before `make lite-dev-up`.

## Shared network

pacod-gw follows the network and naming convention of the pacod products (pacod-id, pacod-ap,
pacod-cms, pacod-kb), [pacod-demo/docs/NETWORK.md](../pacod-demo/docs/NETWORK.md), so that it
runs on its own and as part of the full stack (pacod-demo) without changes:

- Every service joins the external Docker network `pacod-net`; `make network` creates it, and
  every `up` target runs it first.
- Service, container and network name are the same: the core is `pacod-gw`, the other
  services of the gateway are `gw-*` (`gw-console`, `gw-analytics`, `gw-tester`,
  `gw-clickhouse`, `gw-preflight`), and the shared components are `pacod-*`
  (`pacod-postgres`, `pacod-id`, `pacod-caddy`, `pacod-prometheus`, ...).
- `deploy/docker-compose.yml` holds the gateway's own services; the full stack loads it alone.
  `deploy/docker-compose.standalone.yml` adds the gateway's copies of the shared components;
  `make up` and `make dev-up` load both, each with its development override for `dev-up`.
- `platform/` holds what the full stack reads from this repository: the Caddy sites
  (`platform/caddy/gw.caddy`), the PostgreSQL role and database (`platform/postgres/gw-db.sh`)
  and the Prometheus scrape jobs (`platform/prometheus/gw.yml`). The alert rules stay in
  `deploy/prometheus/rules/`, the dashboards in `deploy/grafana/dashboards/`.

Section 19 of the runbook moves the data of an installation made before the rename.

## Logging

pacod-gw logs like the other pacod products,
[pacod-demo/docs/LOGGING.md](../pacod-demo/docs/LOGGING.md): one event per line on standard
output, in JSON (the default) or text, with `timestamp`, `level`, `service`, `logger` and
`message` first and the event's fields flat after them. `RUST_LOG` sets the level of the Rust
services, `PGW_LOG_FORMAT` the format of all of them, `TESTER_LOG_LEVEL` the tester's level;
every compose service rotates its log at 5 × 10 MB. Section 10 of the runbook has the details,
section 8 of [docs/lite.md](docs/lite.md) the minimal gateway's call log.
