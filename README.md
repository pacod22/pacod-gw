# pacod-gw

An AI gateway: a Rust core with a proxy mode and a managed mode, an analytics service on
ClickHouse, a SvelteKit console, and the deployment files to run them.

- [docs/ai-gateway-concept.md](docs/ai-gateway-concept.md): the design.
- [docs/architecture.md](docs/architecture.md): how the code is laid out, what a request goes through, and where the code differs from the design.
- [docs/runbook.md](docs/runbook.md): deployment and operations.
- [docs/testing.md](docs/testing.md): the tester, which runs test cases against a running installation from the console.
- [docs/threat-model.md](docs/threat-model.md): what the gateway protects, how, and what risk remains. Report weaknesses as [SECURITY.md](SECURITY.md) says.

Licensed under the Apache License 2.0 ([LICENSE](LICENSE)).

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
├── tester/                    # the test engine behind the console's Testing page (Python)
├── deploy/                    # Dockerfiles, compose files, Caddy, Helm chart, Prometheus, Grafana
├── scripts/                   # development helpers
├── docs/
└── Makefile
```

## Getting started

Prerequisites: Rust 1.99, Node 24, uv (for the tester) and Docker.

```sh
make test-services   # PostgreSQL and ClickHouse for the tests (ports 25433 and 28123)
make verify          # format, lints, all tests, OpenAPI drift check, console checks

make dev-up          # the whole stack in containers, with a mock provider
make smoke-mock      # an end-to-end flow against the mock
open http://localhost:13000   # the console: admin@example.com / pacod-gw-dev
```

`make dev-up DEV_HTTPS=1` also puts Caddy on ports 80 and 443, as in production:
`https://console.localhost` and `https://api.localhost`, with certificates from Caddy's own
authority (runbook, section 11).

`make smoke-live` makes a few tiny calls to OpenAI and Anthropic through the gateway. It
reads `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` from `.env` (see `.env.example`) and never
prints them.

In the console, platform admins find **Platform → Testing**: a catalog of test cases run against
the gateway by the tester container, in mock mode (the development stack's mock provider) or
live mode (models that are set up). See [docs/testing.md](docs/testing.md).

For production, start from [deploy/.env.example](deploy/.env.example) and the runbook.
