# pacod-gw: notes for working on the code

An AI gateway between applications and LLM providers. Read [README.md](README.md) first,
then [docs/architecture.md](docs/architecture.md) for how the code is laid out. The design is
[docs/ai-gateway-concept.md](docs/ai-gateway-concept.md); where the code differs from it is
listed in the architecture document.

## Two ways to run it

| | Full gateway | Minimal gateway |
|---|---|---|
| What | Core (data plane, management API), analytics, console, tester, pacod-id, PostgreSQL, ClickHouse, Caddy, Prometheus, Grafana | The core's data plane alone, one container, no database |
| Code | `crates/pacod-gw-core` (`pacod-gw-core serve`), `crates/pacod-gw-analytics`, `ui/`, `tester/` | `crates/pacod-gw-lite` on top of `pacod-gw-core` |
| Configuration | Console and management API, stored in PostgreSQL | Environment variables, `deploy/lite/.env` |
| Deployment | `deploy/docker-compose*.yml`, `deploy/node/`, `deploy/helm/` | `deploy/lite/` |
| Documentation | [docs/runbook.md](docs/runbook.md) | [docs/lite.md](docs/lite.md) |

The minimal gateway reuses the request path through a few seams in the core
(`ConfigState::from_rows`, `Gw::standalone`, `dataplane::router_without_objects`,
`relay::CallSink`, `LimitEngine::local`, `Bootstrap::standalone`). A change to the data plane
reaches both; keep those seams working without a database.

## Layout

| Path | What |
|---|---|
| `crates/pacod-gw-core` | The gateway: data plane, management API, configuration snapshot, limits, spool. |
| `crates/pacod-gw-dialects` | Translation between the OpenAI, Anthropic, Gemini and native dialects. Pure functions. |
| `crates/pacod-gw-events` | The contract between core and analytics (usage event, audit record, egress rule). |
| `crates/pacod-gw-analytics` | Ingest, ClickHouse storage, reports, scheduled jobs. |
| `crates/pacod-gw-lite` | The minimal gateway: env parser (`env.rs`), configuration (`config.rs`), configuration rows for the core (`rows.rs`), server and call log. |
| `crates/pacod-gw-testkit` | Not shipped: mock providers (`gw-mock`), smoke test (`gw-smoke`, also `gw-smoke lite`), load generator (`gw-load`). |
| `ui/` | The console (SvelteKit). `tester/`: the test engine behind its Testing page (Python, uv). |
| `authz/gw.yaml` | Roles and permissions; the core's checks and `ui/src/lib/authz.generated.ts` come from it. |
| `vendor/pacod-id` | pacod-id at a pinned commit, copied by `scripts/vendor-pacod-id.sh`. Never edit it here: change pacod-id in its own repository and vendor again. |
| `deploy/` | Dockerfiles, compose files, Caddy, Prometheus, Grafana, Helm chart, scripts. |

## Commands

```sh
make test-services   # PostgreSQL (25433) and ClickHouse (28123) for the tests
make verify          # what CI checks: fmt, clippy -D warnings, all tests, OpenAPI and authz drift, console, tester
make deploy-check    # static checks of deploy/ and .github/

make dev-up          # the full stack from the working tree, with the mock provider
make smoke-mock      # end-to-end flow against it

make lite-dev-up     # the minimal gateway from the working tree, with its mock (18280, 19280)
make lite-smoke      # calls through it, then each call's line in its log
cargo test -p pacod-gw-lite   # needs no database
```

The full Rust suite can run longer than ten minutes. When tests that compare times fail,
compare `docker exec pgw-test-pg date -u` with the host clock before suspecting the code.

## Rules

- **Secrets.** The root `.env` holds real provider keys (for `make smoke-live` and
  `make lite-live`); `deploy/.env` and `deploy/lite/.env` hold production secrets. Never print
  them, and never print `docker compose … config` output that read one of them: it inlines the
  values. Scripts export keys to the process that needs them, never as arguments.
- **Development secrets are fixed.** The development compose files set fixed, public values for
  every secret bound to data; never `${NAME:-default}`, which would let `deploy/.env` in.
- **Migrations.** Never edit a migration in `crates/pacod-gw-core/migrations/` once it exists
  on a branch others use; add a new one. Migrations are additive (an older instance must run
  against a newer schema).
- **Management API changes** need `make openapi` (the console's generated client).
  **Role changes** in `authz/gw.yaml` need `make authz`.
- **Minimal gateway variables.** A new variable goes into `config.rs` (or is rejected as
  unknown) and into `deploy/lite/.env.example`; a test checks that the two agree.
- **Dependencies.** `deny.toml` allows registry sources only; `cargo deny --locked check`.
- **Code style.** `rustfmt.toml` (120 columns). Comments say why, in full sentences.
- **Documentation** is British English (organisation, behaviour), with short declarative
  sentences, tables, and numbered sections in the long documents. Keep the runbook, the
  architecture document and `docs/lite.md` true to the code when it changes.
- **Commits** are titled `<Area>[, stage N]: <what changed>` and say why in the body.
