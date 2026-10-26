# pacod-gw: notes for working on the code

An AI gateway between applications and LLM providers. Read [README.md](README.md) first,
then [docs/architecture.md](docs/architecture.md) for how the code is laid out. The design is
[docs/ai-gateway-concept.md](docs/ai-gateway-concept.md); where the code differs from it is
listed in the architecture document.

## Two ways to run it

| | Full gateway | Minimal gateway |
|---|---|---|
| What | Core (data plane, management API), analytics, console, tester, pacod-id, PostgreSQL, ClickHouse, Caddy, Prometheus, Grafana | The core's data plane alone, one container, no database |
| Compose services | `pacod-gw` (the core), `gw-analytics`, `gw-console`, `gw-tester`, `gw-clickhouse`, `gw-preflight`; shared: `pacod-postgres`, `pacod-db-init`, `pacod-id`, `pacod-caddy`, `pacod-otel`, `pacod-prometheus`, `pacod-alertmanager`, `pacod-grafana` | `pacod-gw` |
| Code | `crates/pacod-gw-core` (`pacod-gw-core serve`), `crates/pacod-gw-analytics`, `ui/`, `tester/` | `crates/pacod-gw-lite` on top of `pacod-gw-core` |
| Configuration | Console and management API, stored in PostgreSQL | Environment variables, `deploy/lite/.env` |
| Deployment | `deploy/docker-compose*.yml`, `deploy/node/`, `deploy/load-agent/`, `deploy/helm/`, `platform/` | `deploy/lite/` |
| Documentation | [docs/runbook.md](docs/runbook.md) | [docs/lite.md](docs/lite.md) |

The minimal gateway reuses the request path through a few seams in the core
(`ConfigState::from_rows`, `Gw::standalone`, `dataplane::router` with `Gw::objects`,
`store::objects::ObjectStore`, `Gw::record_event`, `relay::CallSink`, `LimitEngine::local`,
`Bootstrap::standalone`). A change to the data plane reaches both; keep those seams working
without a database.

## Layout

| Path | What |
|---|---|
| `crates/pacod-gw-core` | The gateway: data plane, management API, configuration snapshot, limits, spool. |
| `crates/pacod-gw-dialects` | Translation between the OpenAI, Anthropic, Gemini and native dialects. Pure functions. |
| `crates/pacod-gw-events` | The contract between core and analytics (usage event, audit record, egress rule). |
| `crates/pacod-gw-analytics` | Ingest, ClickHouse storage, reports, scheduled jobs. |
| `crates/pacod-gw-lite` | The minimal gateway: env parser (`env.rs`), configuration (`config.rs`), configuration rows for the core (`rows.rs`), server and call log. |
| `crates/pacod-gw-load` | The load engine and the load agent (`pacod-gw-load-agent`, an image) behind the console's load tests; `tests/contract/` is the JSON contract with the tester. |
| `crates/pacod-gw-logging` | The log of every binary: the event of [pacod-demo/docs/LOGGING.md](../pacod-demo/docs/LOGGING.md) in JSON or text, level and format from the environment. |
| `crates/pacod-gw-testkit` | Not shipped: mock providers (`gw-mock`), smoke test (`gw-smoke`, also `gw-smoke lite`), load generator (`gw-load`, on `pacod-gw-load`'s engine). |
| `ui/` | The console (SvelteKit). `tester/`: the test engine behind its Testing page (Python, uv), load tests included. |
| `authz/gw.yaml` | Roles and permissions; the core's checks and `ui/src/lib/authz.generated.ts` come from it. |
| `vendor/pacod-id` | pacod-id at a pinned commit, copied by `scripts/vendor-pacod-id.sh`. Never edit it here: change pacod-id in its own repository and vendor again. |
| `deploy/` | Dockerfiles, compose files, Caddy, Prometheus, Grafana, Helm chart, scripts. `docker-compose.yml` is the app file (the gateway's own services), `docker-compose.standalone.yml` its copies of the shared components; each has a `.dev.yml` override. |
| `platform/` | The gateway's fragments of the shared configuration, read by its own stack and by the full stack (pacod-demo): `caddy/gw.caddy` (sites), `postgres/gw-db.sh` (role and database), `prometheus/gw.yml` (scrape jobs). |

## Commands

```sh
make test-services   # PostgreSQL (25433), ClickHouse (28123) and Valkey (26379) for the tests
make verify          # every check before a commit: fmt, clippy -D warnings, all tests, OpenAPI and authz drift, console, tester
make deploy-check    # static checks of deploy/

make dev-up          # the full stack from the working tree, with the mock provider
make smoke-mock      # end-to-end flow against it

make lite-dev-up     # the minimal gateway from the working tree, with its mock (18280, 19280); not with dev-up
make load-agent-dev-up   # a second load agent playing another machine (18196); dev-up has one
make lite-smoke      # calls through it, then each call's line in its log
cargo test -p pacod-gw-lite   # needs no database
```

The full Rust suite can run longer than ten minutes. When tests that compare times fail,
compare `docker exec pgw-test-pg date -u` with the host clock before suspecting the code.

## Rules

- **Shared networks.** Every compose service joins the external network `pacod-net`, and
  its service key, `container_name` and DNS name are the same: `gw-*` for the gateway's own
  services, `pacod-*` for shared components, volumes likewise
  ([pacod-demo/docs/NETWORK.md](../pacod-demo/docs/NETWORK.md)). Listeners that only servers
  reach are on the second external network, `pacod-admin-net` (internal, no route out): the
  management API and pacod-id's internal listener. Each binds to an alias its service has on
  `pacod-admin-net` alone (`pacod-gw-admin:8081`, `pacod-id-internal:8071`), never to
  `0.0.0.0`, and its clients join that network and call the alias, never `pacod-gw:8081` or
  `pacod-id:8071`. A development file that publishes such a port keeps `0.0.0.0`, because a
  published port never reaches an internal network. The overlays that span machines or
  isolate the load agent keep their extra networks. An app-file `depends_on` on a
  shared component carries `required: false`. Prometheus job names and instance ids
  (`pacod-gw-core`, `otel-collector`, `core`, `gateway-1`) never change. Never print
  `docker compose ... config` output that read `deploy/.env`; `config -q` is fine.
- **Logging.** Every binary logs through `pacod_gw_logging` (`init`, or `install` with its
  `Settings`), the tester through `pgw_tester.logs`; nothing sets up a subscriber of its own.
  Every compose service carries `logging: *logging` (an `x-logging` anchor per file), and the
  gateway's own services `SERVICE_NAME`; log variables are passed as `${NAME:-<default>}`
  ([pacod-demo/docs/LOGGING.md](../pacod-demo/docs/LOGGING.md)).

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
- **Load agent contract.** A change to `crates/pacod-gw-load/src/contract.rs` changes the
  fixtures in `crates/pacod-gw-load/tests/contract/` and `tester/src/pgw_tester/load/contract.py`
  together; the tests of both sides read the fixtures.
- **Minimal gateway variables.** A new variable goes into `config.rs` (or is rejected as
  unknown) and into `deploy/lite/.env.example`; a test checks that the two agree.
- **Dependencies.** `deny.toml` allows registry sources only; `cargo deny --locked check`.
- **Code style.** `rustfmt.toml` (120 columns). Comments say why, in full sentences.
- **Documentation** is British English (organisation, behaviour), with short declarative
  sentences, tables, and numbered sections in the long documents. Keep the runbook, the
  architecture document and `docs/lite.md` true to the code when it changes.
- **Commits** are titled `<Area>[, stage N]: <what changed>` and say why in the body.
