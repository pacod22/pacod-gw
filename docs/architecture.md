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
query after checking the caller's role (section 10.3).

## Crates

| Crate | Role |
| --- | --- |
| `pacod-gw-core` | Library and binary. `serve --role all\|gateway\|admin`, `migrate`, `openapi`. |
| `pacod-gw-dialects` | The provider-neutral request, response and stream model, and the translators between it and the OpenAI, Anthropic and native dialects. Pure functions, no I/O. |
| `pacod-gw-events` | The contract between core and analytics: usage event, audit record, ingest batch, job lease. |
| `pacod-gw-analytics` | Ingest, ClickHouse storage and the report queries. See its `API.md`. |
| `pacod-gw-testkit` | Not shipped. Scripted mock providers (`gw-mock`), the smoke test (`gw-smoke`) and the load generator (`gw-load`). |

`ui/` is the console. It holds a client generated from the core's OpenAPI document and calls
the management API from server-side code only.

## Inside the core

| Module | What it does |
| --- | --- |
| `bootstrap` | Start-up values from the environment and an optional file: listeners, database, keys. Everything else is configuration in PostgreSQL. |
| `store` | The only SQL. Configuration objects are typed JSON documents in one table; a change writes its rows, its audit entry and a version increment in one transaction. |
| `model` | The types of those documents: organisations, workspaces, keys, providers, credentials, models, grants, prices, templates, limits. |
| `snapshot` | The immutable in-memory configuration the request path reads. A change swaps in a new snapshot; other instances poll the version every second. |
| `mgmt` | The management API: a small CRUD framework (`crud.rs`), one file per resource, sessions and roles, the OpenAPI document, the analytics facade, the event feed, the playground. |
| `dataplane` | The request path. `proxy.rs` (proxy mode); `managed.rs` (the one pipeline of managed mode, also token counting and speech); `openai.rs` and `anthropic.rs` (the two same-dialect routes); `translate.rs` (the route through the neutral model); `native.rs` (model list, quota); `common.rs` (gateway parameters, admission, headers, sending with retries); `shape.rs` (reads and writes governed parameters per dialect). |
| `json` | A scanner that finds the top-level members of a body without building a tree, and a patcher that rewrites named members and returns everything else as slices of the original bytes. |
| `resolve`, `params` | Which model serves a request (templates, grants, life cycle), and how template, caller and capability profile combine. |
| `limits` | Limit counters in memory, flushed to PostgreSQL and read back about once a second. |
| `cost` | Cost of a call from its usage and a price version. |
| `upstream` | The pooled HTTPS client to providers and the egress policy. |
| `relay` | The response body that hands each provider chunk to the client and a copy to the usage tap; the timeline behind the gateway-delay figures; the recorder that settles every call exactly once. |
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
connection errors are retried before the first byte. The interface and the model's provider
decide the route:

| Caller speaks | Model is at | Route |
| --- | --- | --- |
| OpenAI dialect | an OpenAI-compatible provider | Same dialect: the caller's bytes are patched. The answer gets the cost inside `usage`; a stream is shaped event by event. |
| Anthropic dialect | Anthropic | Same dialect: `system`, `tools` and `messages` reach Anthropic as the bytes the caller sent, and the answer comes back as Anthropic wrote it. The cost is in a header. |
| anything else, and always the native interface | either family | Translated: client half of the caller's dialect → neutral model → provider half of the provider's dialect, and back. Deterministic: the same provider answer gives the same bytes. |

`/v1/messages/count_tokens`, `/v1/audio/speech` and `/gw/v1/speech` use the same resolution,
admission and sending, with one provider call and nothing to translate.

Not built: Anthropic's Message Batches and Files with workspace ownership (concept section
8.3 names them as the first items to defer; proxy mode with a workspace's own credential
relays them).

## What is stored where

| Data | Store |
| --- | --- |
| Configuration, its versions and the audit log | PostgreSQL |
| Limit counters | Memory, synchronised through PostgreSQL |
| Usage events and audit records | Spool on the core's disk, then ClickHouse |
| Provider credentials | PostgreSQL, sealed with a data key wrapped by the key-encryption key from the environment |
| API keys and admin tokens | PostgreSQL, as HMAC-SHA256 with a pepper from the environment; the secret is shown once |

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
