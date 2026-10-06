# Architecture

This page describes the code as it is built. The design and the reasons behind it are in
[ai-gateway-concept.md](ai-gateway-concept.md); section numbers below refer to it.

```text
                         ┌──────────────────────────────┐
 applications ─────────▶ │        pacod-gw-core         │ ─────────▶ LLM providers
  /proxy/{provider}/…    │  data plane   :8080          │
  /v1/…   /gw/v1/…       │  management   :8081  ◀────── console (SvelteKit, server side only)
                         │  metrics      :9100  ◀────── Prometheus ──▶ Grafana
                         └───┬──────────────┬───────────┘
              configuration  │              │ usage events and audit records
                             ▼              ▼ (spool on disk, shipped in batches)
                        PostgreSQL    pacod-gw-analytics :8090 ──▶ ClickHouse
```

The core is the only component applications and the console talk to. The analytics service
is reached only by the core; the console asks the core for reports, and the core forwards the
query after checking the caller's role (section 10.3). In the other direction the analytics
service asks the core for scheduled work (the price tracker) and posts back what it found;
the core decides what to do with it.

## Crates

| Crate | Role |
| --- | --- |
| `pacod-gw-core` | Library and binary. `serve --role all\|gateway\|admin`, `migrate`, `openapi`. |
| `pacod-gw-dialects` | The provider-neutral request, response and stream model, and the translators between it and the OpenAI, Anthropic and native dialects. Pure functions, no I/O. |
| `pacod-gw-events` | The contract between core and analytics: usage event, audit record, ingest batch, job lease. Also the one rule both apply alike: which addresses may be connected to on an administrator's word (`egress`). |
| `pacod-gw-analytics` | Ingest, ClickHouse storage, the report queries, and the runner for scheduled jobs (`jobs/`). See its `API.md`. |
| `pacod-gw-testkit` | Not shipped. Scripted mock providers (`gw-mock`), the smoke test (`gw-smoke`) and the load generator (`gw-load`). |

`ui/` is the console. It holds a client generated from the core's OpenAPI document and calls
the management API from server-side code only.

## Inside the core

| Module | What it does |
| --- | --- |
| `bootstrap` | Start-up values from the environment and an optional file: listeners, database, keys. Everything else is configuration in PostgreSQL. |
| `store` | The only SQL. Configuration objects are typed JSON documents in one table; a change writes its rows, its audit entry and a version increment in one transaction. |
| `model` | The types of those documents: organisations, workspaces, keys, providers, credentials, models, grants, prices, templates, limits, settings, the price job. |
| `snapshot` | The immutable in-memory configuration the request path reads. A change swaps in a new snapshot; other instances poll the version every second. |
| `mgmt` | The management API: a small CRUD framework (`crud.rs`), one file per resource, sessions and roles, the OpenAPI document, the analytics facade (`facade.rs`: reports, request log, audit records), monitoring (`monitoring.rs`: live figures, provider health, open alerts), the price tracker's configuration, runs and proposals (`resources/pricejob.rs`), configuration export and import (`transfer.rs`), the event feed, the playground. |
| `dataplane` | The request path. `proxy.rs` (proxy mode); `managed.rs` (the one pipeline of managed mode, also token counting and speech); `openai.rs` and `anthropic.rs` (the two same-dialect routes); `translate.rs` (the route through the neutral model); `native.rs` (model list, quota); `common.rs` (gateway parameters, admission, headers, sending with retries); `shape.rs` (reads and writes governed parameters per dialect). |
| `json` | A scanner that finds the top-level members of a body without building a tree, and a patcher that rewrites named members and returns everything else as slices of the original bytes. |
| `resolve`, `params` | Which model serves a request (templates, grants, life cycle), and how template, caller and capability profile combine. |
| `routing` | Routing among the models of a template (section 7.5): `compile.rs` turns a scheme into typed predicates (the same code validates it in the API), `select.rs` binds it to each template's pool when the snapshot is built and orders the candidates of a call, `live.rs` holds what each instance learns per model (circuit breaker, calls in flight, time to first byte) outside the snapshot, `cel.rs` the optional CEL conditions (cargo feature `routing-cel`, on by default). |
| `limits` | Limit counters in memory, flushed to PostgreSQL and read back about once a second. |
| `cost` | Cost of a call from its usage and a price version. |
| `upstream` | The pooled HTTPS client to providers. It resolves names itself and refuses addresses the egress policy does not allow. |
| `relay` | The response body that hands each provider chunk to the client and a copy to the usage tap; the timeline behind the gateway-delay figures; the recorder that settles every call exactly once; the audit capture (`audit.rs`) that, in audit mode, keeps the four views of a call. |
| `extract`, `sse` | Usage extraction per dialect; server-sent event framing. |
| `spool` | The durable event spool and its shipper. |
| `telemetry` | Prometheus metrics, traces over OTLP/HTTP, the live figures. |

## A request, step by step

**Proxy mode** (`/proxy/{provider}/…`, section 4.2):

1. Authenticate the key against the snapshot (an HMAC lookup, no database).
2. Buffer and scan the body. Read `model`, `stream`, `stream_options`.
3. Resolve the target: a template (`@name` or `x-gw-template`), or the provider's model id,
   which must belong to a repository model granted to the workspace.
4. With a template: write its model and parameters into the scanned body (`shape.rs`), or
   check the request against it. Content members keep their bytes.
5. Admission: the key's concurrency cap, then every limit that applies.
6. Replace the credential and send. One retry, only if nothing was sent.
7. Relay the response unchanged; the tap reads usage from a copy.
8. Settle: usage, cost, limit counters, metrics, the usage event.

**Managed mode** (section 4.3) serves three interfaces with one pipeline: `/v1/chat/completions`
(OpenAI-compatible), `/v1/messages` (Anthropic-compatible) and `/gw/v1/chat` (native). It
differs from proxy mode in steps 2 to 7: gateway parameters also come from the body's
`gateway` object; the caller's own parameters pass the capability filter; `429`, `5xx` and
connection errors are retried before the first byte. For a template with several models,
routing orders them once (`resolve_routed`), and `run` in `managed.rs` tries them in turn:
limits are admitted per model (`admit_limits`, the key's concurrency slot once), the body
is shaped again for each model's dialect, and a failure the scheme names moves the call on
before the first byte reaches the caller. The interface and the model's provider
decide the route:

| Caller speaks | Model is at | Route |
| --- | --- | --- |
| OpenAI dialect | an OpenAI-compatible provider | Same dialect: the caller's bytes are patched. The answer gets the cost inside `usage`; a stream is shaped event by event. |
| Anthropic dialect | Anthropic | Same dialect: `system`, `tools` and `messages` reach Anthropic as the bytes the caller sent, and the answer comes back as Anthropic wrote it. The cost is in a header. |
| anything else, and always the native interface | either family | Translated: client half of the caller's dialect → neutral model → provider half of the provider's dialect, and back. Deterministic: the same provider answer gives the same bytes. |

`/v1/messages/count_tokens`, `/v1/audio/speech` and `/gw/v1/speech` use the same resolution,
admission and sending, with one provider call and nothing to translate.

Anthropic's Message Batches and Files are not gateway resources; see the differences below.

## Audit mode, prices, monitoring

**Audit mode** (section 14.6). `relay/audit.rs` decides per call whether it is recorded:
the platform's and the organisation's rule, the workspace's settings, the template's
switch, and what the call asked for. A recorded call keeps four views: the caller's request,
the provider request of every attempt, the provider's answers, and what the caller received.
A body that equals another is stored once and referred to. Credentials are removed before
anything is kept. The record is written when the call settles, into a spool of its own, and
the usage event of the call names it. `GET /requests/{id}/audit` returns it; `{id}` is the
event's id or the value of `x-gw-request-id`.

**Gateway delay** (section 14.5). Every usage event carries the stages of the call's
timeline: time before the provider was called, time after its answer, and their sum, the
delay the gateway caused. The response carries it in `x-gw-delay-ms`, the request log and
`/monitoring/delay` report it from the stored events, and Prometheus has it as the
histogram `gw_delay_seconds`.

**Price tracker** (section 13.3). The job's configuration, its runs and the decisions live
in the core (`mgmt/resources/pricejob.rs`, tables `jobs` and `job_runs`); the fetching runs
in the analytics service (`jobs/prices.rs`), which takes a lease, reads the sources and
posts candidates. Sources are fetched under the egress policy, redirects included. Per
model the core then leaves a hand-entered price alone, confirms an
unchanged one, applies a change within the bound as a new price version, or stores a
proposal for approval. A pricing page is read by a model through the gateway's own data
plane, with a template of a workspace the job names.

**Monitoring** (sections 14.7 to 14.9). Live figures and provider health come from counters
in memory (`telemetry/live.rs`). Open alerts are computed on request from the same state
(`/monitoring/alerts`): limits, prices, proposals, providers, spools. The alert rules for
Prometheus in `deploy/prometheus/rules/` read the exported series, among them
`gw_limit_used_ratio`, `gw_price_age_days` and `gw_upstream_health`.

## What is stored where

| Data | Store |
| --- | --- |
| Configuration, its versions and the audit log; job leases and runs | PostgreSQL |
| Limit counters | Memory, synchronised through PostgreSQL |
| Usage events and audit records | Spool on the core's disk, then ClickHouse |
| Provider credentials | PostgreSQL, sealed with a data key wrapped by the key-encryption key from the environment |
| API keys and admin tokens | PostgreSQL, as HMAC-SHA256 with a pepper from the environment; the secret is shown once |

## Where the code differs from the concept

The concept is the design; this is what was built differently, or not built. Section
numbers are the concept's.

| Area | In the code |
| --- | --- |
| Configuration tables (appendix A) | One table of typed JSON documents (`config_objects`) with a kind, a parent and a name, instead of one table per kind. The types are in `model/`. |
| Grants (7) | A workspace without grants of its own uses its organisation's. |
| Proxy mode and the repository (4.2) | A call that names a model is served only if that model is in the repository and granted to the workspace. Paths without a model (a provider's model list, for example) are relayed as they are. |
| `x-gw-cost` (8.1) | Sent on non-streaming responses of managed mode. Proxy mode relays the answer while it arrives and learns the usage at its end; the cost is in the request log under the request id. |
| Anthropic Message Batches and Files (8.3) | Not built as gateway resources with workspace ownership. Proxy mode relays those paths. |
| Price variants (13.1) | Stored per price (`batch`, `fast`); the gateway does not yet pick a variant from what the provider reports, so calls are costed at the standard rates. |
| Spend limits (12) | Count in the currency of the prices. There is no currency conversion. |
| `usage_policy: require` (4.2) | Set per key. It is not switched on by itself when a token limit applies. |
| Model discovery (6) | On request (`GET /providers/{id}/discovery`). There is no scheduled discovery run and no list of proposals. |
| Price extraction from a page (13.3) | Uses a workspace and a template the price job names, not a fixed `_system` workspace. |
| Rollup tables (14.4) | Not built. Every report reads the event table, which removes duplicates on read. Measured on a development machine with 5 million events in 30 days: 0.05 to 0.55 s per report. Rollups become worth their complexity at roughly a hundred times that. |
| Errors in reports (14.4) | As the concept defines them: every status other than `ok`, which includes calls the gateway refused (`rejected`) and calls the client cancelled. The Prometheus alert on the error rate leaves cancelled calls out. |
| Audit record (14.6) | `resolution` names the template, its version, what was dropped and adjusted, and the limits checked. It does not say for each parameter whether the value came from the template or the caller. Speech and token-counting calls are not recorded. |
| Metric names (14.7) | `gw_cost_micros_total` (millionths of the currency, so the counter is an integer) and `gw_config_apply_lag_versions` (versions behind, not seconds). |
| Translated answers (8.5) | Byte-stable for the same provider answer, except `created` in OpenAI-shaped answers, which is the gateway's clock. |
| Roles (5.4) | An organisation admin changes everything on the organisation (display name, settings, locks, audit rule, routing assignment) except its name and whether it is enabled, which stay with platform admins. Anyone with a role in a workspace may read which models its organisation has. Deleting a user, an organisation or a workspace removes the role bindings that name it. |
| Routing (7.5) | Breakers, round-robin turns and latency averages are per instance; only managed calls feed them. Proxy mode routes among the pool's models at the URL's provider but does not fail over. Token counting and speech take the first model routing names and do not fail over. Prices for `cheapest` are those in effect when the snapshot was built. The input size for `input_tokens` and the context-window check is the body size divided by four. |

## Ports

| Service | In the container | Development stack (`make dev-up`) |
| --- | --- | --- |
| Core, data plane | 8080 | 18080 |
| Core, management API | 8081 | 18081 |
| Core, metrics | 9100 | 19100 |
| Analytics | 8090 | 18090 |
| Console (through Caddy) | 3000 | 13000 |
| Mock provider | 9000 | 19000 |
| PostgreSQL | 5432 | 15433 |
| ClickHouse HTTP | 8123 | 18123 |
| Prometheus | 9090 | 19090 |
| Grafana | 3000 | 13001 |

The tests use their own PostgreSQL and ClickHouse on 25433 and 28123 (`make test-services`).

## Working on it

```sh
make test-services   # PostgreSQL and ClickHouse for the tests
make verify          # format, lints, all tests, OpenAPI drift, console checks
make dev-up          # the whole stack in containers, with the mock provider
make smoke-mock      # the end-to-end flow against the mock
make smoke-live      # a few tiny calls with the real keys in .env
make load PROFILE=proxy
make deploy-check    # static checks of deploy/ and the CI workflow
```

[runbook.md](runbook.md) covers deployment and operations.
