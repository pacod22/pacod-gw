# pacod-gw

An AI gateway: a Rust core with a proxy mode and a managed mode, an analytics service on
ClickHouse, a SvelteKit console, and the deployment files to run them.

- [docs/ai-gateway-concept.md](docs/ai-gateway-concept.md): the design.
- [docs/architecture.md](docs/architecture.md): how the code is laid out, what a request goes through, and where the code differs from the design.
- [docs/runbook.md](docs/runbook.md): deployment and operations.

## Layout

```text
pacod-gw/
├── crates/
│   ├── pacod-gw-core/         # the gateway: data plane, management API, configuration
│   ├── pacod-gw-dialects/     # the neutral model and the OpenAI, Anthropic and native translators
│   ├── pacod-gw-events/       # the contract between core and analytics
│   ├── pacod-gw-analytics/    # ingest, ClickHouse storage, reports, scheduled jobs
│   └── pacod-gw-testkit/      # mock providers, smoke test, load generator (not shipped)
├── ui/                        # the console
├── deploy/                    # Dockerfiles, compose files, Caddy, Helm chart, Prometheus, Grafana
├── scripts/                   # development helpers
├── docs/
└── Makefile
```

## Getting started

Prerequisites: Rust 1.99, Node 24 and Docker.

```sh
make test-services   # PostgreSQL and ClickHouse for the tests (ports 25433 and 28123)
make verify          # format, lints, all tests, OpenAPI drift check, console checks

make dev-up          # the whole stack in containers, with a mock provider
make smoke-mock      # an end-to-end flow against the mock
open http://localhost:13000   # the console: admin@example.com / pacod-gw-dev
```

`make smoke-live` makes a few tiny calls to OpenAI and Anthropic through the gateway. It
reads `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` from `.env` (see `.env.example`) and never
prints them.

For production, start from [deploy/.env.example](deploy/.env.example) and the runbook.
