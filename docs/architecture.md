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

People sign in to the console in one of two ways (`PGW_AUTH`). With `local`, the default, the
core keeps users, sessions, role bindings and admin tokens itself. With `oidc` they belong to
pacod-id, a separate OpenID Connect provider vendored in `vendor/pacod-id` and run as a
service of its own (`pacod-id`): the console signs people in there, the core checks pacod-id's
tokens and follows its feed of roles, and the data plane never asks it.

```text
 browser ──▶ console (BFF) ──▶ core :8081 ──▶ pacod-id :8071   internal: feed, introspection,
    │            │                              ▲   ▲            admin API, token exchange
    │            └──────────────────────────────┘   │            (console: tokens, sign-out)
    │                              tester ──────────┘            (tester: delegated grants)
    └──────────▶ pacod-id :8070   sign-in pages, through Caddy on PGW_ID_HOST
```

Load tests (testing.md, section 9) add the load agents. The tester, the engine behind the
console's Testing page, sets up what a test needs through the management API and drives the
agents over HTTP. The agents only make calls, to this gateway's data plane or to another
gateway, and report each second. They run in containers of their own, in the stack or on other
machines.

```text
 console ──▶ tester :8095 ──▶ core :8081   set-up, clean-up, the gateway's own figures
                  │
                  └──▶ pacod-gw-load-agent :8096 (one or more) ──▶ data plane :8080, or an
                         plan, start, a report a second             external gateway's URL
```

The same data plane also runs on its own as the minimal gateway, `pacod-gw-lite`
([lite.md](lite.md)): one process without PostgreSQL, analytics service, management API,
console or metrics. It reads its configuration from environment variables, builds from them
the configuration rows the core's snapshot is built from, and writes every call to its log.

```text
                         ┌──────────────────────────────┐
 applications ─────────▶ │        pacod-gw-lite         │ ─────────▶ LLM providers
  /proxy/{provider}/…    │  the core's data plane :8080 │
  /v1/…   /gw/v1/…       │  snapshot from .env, limits  │ ─────────▶ stdout: one line per call
                         │  in memory                   │
                         └──────────────────────────────┘
```

## Crates

| Crate | Role |
| --- | --- |
| `pacod-gw-core` | Library and binary. `serve --role all\|gateway\|admin`, `migrate`, `openapi`. |
| `pacod-gw-dialects` | The provider-neutral request, response and stream model, and the translators between it and the OpenAI, Anthropic and native dialects, plus the provider half for Gemini's own API (`gemini.rs`). `assemble.rs` folds a stream's events into a complete answer and turns one back into events, for the response cache. Pure functions, no I/O. Tested against the recorded provider answers in `pacod-gw-testkit/fixtures/` (`tests/recorded.rs`). |
| `pacod-gw-events` | The contract between core and analytics: usage event, audit record, ingest batch, job lease. Also the one rule both apply alike: which addresses may be connected to on an administrator's word (`egress`). |
| `pacod-gw-analytics` | Ingest, ClickHouse storage, the report queries, and the runner for scheduled jobs (`jobs/`: the price tracker, usage-event retention, the rollups). See its `API.md`. |
| `pacod-gw-lite` | Library and binary of the minimal gateway ([lite.md](lite.md)): reads its configuration from environment variables, turns it into the configuration rows the core's snapshot is built from (`rows.rs`, the intent plugin's install, catalogue and binding and the response cache's install among them), and serves the core's data plane without a database (`Gw::standalone`, `dataplane::router`). Every settled call reaches its log through the core's `CallSink`. |
| `pacod-gw-plugin` | The contract between the core and its plugins ([plugin concept](plugin-concept.md)): the `Plugin` and `Bound` traits, the hooks a manifest names (`on_request`, `answers`), what a plugin sees of a call (`RequestInput`, the conversation as text; `PlannedCall`, the request as the provider receives it), what it answers (`Verdict`, `Signals`; `Keep` for an answering plugin), and the `Host` through which it asks the core for model calls. Pure types, no I/O. |
| `pacod-gw-plugin-cache` | The response cache plugin ([plugin concept](plugin-concept.md), section 15): the settings of a binding (`settings.rs`), the key and which calls may be answered again (`key.rs`), and which answers are kept (`lib.rs`). Policy only: the core reads and writes the store. Installed by default; the core's cargo feature `plugin-cache` (on by default) registers it. |
| `pacod-gw-plugin-intent` | The intent plugin ([plugin concept](plugin-concept.md), section 5): the settings of a binding (`settings.rs`), the intent catalogue, its rules and the built-in template a new one starts from (`catalogue.rs`, `rules.rs`), what part of a conversation is classified (`select.rs`), the LLM engine's prompt, schema and answer (`llm.rs`), the Jev engine's typed questions and answer (`jev.rs`), the cascade (`lib.rs`), and the answer cache (`cache.rs`). Its types are always compiled; the core's cargo feature `plugin-intent` (on by default) registers it. |
| `pacod-gw-load` | The load engine and the load agent (`pacod-gw-load-agent`, shipped as an image). The engine makes calls at a concurrency or a rate that follows stages and records each second; latencies are integer log-linear histograms (`histogram.rs`), which add up across seconds and agents. `contract.rs` is the JSON between tester and agent; `tests/contract/` holds the fixtures both sides test against. See its `README.md`. |
| `pacod-gw-logging` | The log of every binary, after the pacod logging convention (`pacod-demo/docs/LOGGING.md`): the event in JSON or text (`timestamp`, `level`, `service`, `logger`, `message`, then the fields flat), the level from `RUST_LOG`, the format from each binary's variable, `service` from `SERVICE_NAME`. |
| `pacod-gw-testkit` | Not shipped. Scripted mock providers (`gw-mock`), the smoke test (`gw-smoke`) and the load generator (`gw-load`, on the engine of `pacod-gw-load`). |

`ui/` is the console. It holds a client generated from the core's OpenAPI document and calls
the management API from server-side code only. With `PGW_AUTH=oidc` it is also a
confidential client of pacod-id that keeps the tokens on its server side (a BFF):
`@pacod/id-sveltekit`, vendored with pacod-id, serves `/auth/login`, `/auth/callback` and
`/auth/logout` and seals the tokens in one cookie (`pgw_session`, `__Host-` on https); the
access token is what the console sends to the core (`ui/src/lib/server/identity.ts`). The
roles and permissions are one file, `authz/gw.yaml`, from which the core's checks and the
console's `authz.generated.ts` are made; `authz/vectors/` pins what each role may do.

`vendor/pacod-id` holds pacod-id at a pinned commit (`vendor/pacod-id/VERSION`): its crates
are path dependencies (the core uses `pacod-id-client` and `pacod-id-authz`; the tests start
`pacod-id` itself in process), its chart is the Helm chart's subchart `id`, and its image is
built from it. It has its own documents in `vendor/pacod-id/docs/`.

## Inside the core

| Module | What it does |
| --- | --- |
| `bootstrap` | Start-up values from the environment and an optional file: listeners, database, keys. Everything else is configuration in PostgreSQL. `Bootstrap::standalone` is the minimal gateway's: the file's defaults, no database. |
| `identity` | `PGW_AUTH=oidc` (`src/identity.rs`, not `cluster/identity.rs`): the client of pacod-id, the copy of its bindings kept current by its feed, the check of a caller's token (a JWT, or `pidt_`/`pgwa_` by introspection; `pgws_` refused), the push of `authz/gw.yaml` at start, the identity outbox's deliverer and sweep, and forgetting accounts pacod-id deleted. Its copy older than `PGW_ID_MAX_STALENESS` makes management calls answer 503 `identity_unavailable`. `identity_migration.rs`: `identity export`, `rollback` and `finalize`. |
| `cluster` | Several instances on one database. `identity.rs`: the instance id, kept in the spool directory, and the lock on that directory. `mod.rs`: taking the instance's row in the registry at start, refused while another running process holds the id. `report.rs`: the live figures every instance publishes into its row with the heartbeat, as raw counts the management API can add up, the response cache's store among them. |
| `store` | The only SQL. Configuration objects are typed JSON documents in one table; a change writes its rows, its audit entry and a version increment in one transaction. The record of files and batches is reached through the trait `objects::ObjectStore`, so that a gateway without the database can keep it elsewhere. |
| `model` | The types of those documents: organisations, workspaces, keys, providers, credentials, models, grants, prices, templates, limits, settings, the price job. |
| `snapshot` | The immutable in-memory configuration the request path reads. A change swaps in a new snapshot; other instances poll the version every second and read only the rows the change log names (`reload.rs`), everything after a restore or a gap. Without a database (`ConfigState::from_rows`, the minimal gateway) the snapshot is built once from rows held in memory and never changes; `store()` is then `None`. The response cache's clears (`answer_clears`, each workspace's `answers_cleared_at`) reach every instance this way. |
| `mgmt` | The management API: a small CRUD framework (`crud.rs`), one file per resource, sessions and roles, the OpenAPI document, the analytics facade (`facade.rs`: reports, request log, audit records), monitoring (`monitoring.rs`: live figures, provider health, open alerts), the price tracker's configuration, runs and proposals (`resources/pricejob.rs`), configuration export and import (`transfer.rs`), the event feed, the playground, the plugin installs, bindings, intent catalogues and their templates (`resources/plugin.rs`) the intent plugin's try, evaluate and demo endpoints (`resources/intent_try.rs`), and the response cache's status and clears (`cache.rs`). In OIDC mode `idfacade.rs` answers the paths of users, role bindings, admin tokens, members and invitations from pacod-id's admin API, called with the caller's token exchanged for one for pacod-id: the gateway's check first, then pacod-id's rules, in the gateway's shapes. |
| `dataplane` | The request path. `client_ip.rs` (the client's address behind a trusted load balancer); `proxy.rs` (proxy mode); `tools.rs` (the provider tools a workspace allows, checked on what the provider receives); `managed.rs` (the one pipeline of managed mode, also token counting and speech); `openai.rs` and `anthropic.rs` (the two same-dialect routes); `translate.rs` (the route through the neutral model); `responses.rs` (OpenAI's Responses API on the same-dialect route, and the paths that read and delete a kept response); `state.rs` (the responses a workspace keeps: a call that continues one expanded, the answer stamped and written before it is released); `native.rs` (model list, quota); `common.rs` (gateway parameters, admission, headers, sending with retries); `shape.rs` (reads and writes governed parameters per dialect). `Gw::standalone` is the data plane without a database: no spools, no traces. `router` serves the files and batches paths (`objects.rs`) where `Gw::objects` holds a record of objects (`store::objects::ObjectStore`: PostgreSQL in the full gateway), and answers 501 on them otherwise. Usage events the gateway makes itself, such as a batch item's, go out through `Gw::record_event`: to the spool, and to the call sink of a gateway without one. |
| `json` | A scanner that finds the top-level members of a body without building a tree, and a patcher that rewrites named members and returns everything else as slices of the original bytes. |
| `resolve`, `params` | Which model serves a request (templates, grants, life cycle), and how template, caller and capability profile combine. |
| `routing` | Routing among the models of a template (section 7.5): `compile.rs` turns a scheme into typed predicates (the same code validates it in the API), `select.rs` binds it to each template's pool when the snapshot is built and orders the candidates of a call, `live.rs` holds what each instance learns per model (circuit breaker, calls in flight, time to first byte) outside the snapshot, `cel.rs` the optional CEL conditions (cargo feature `routing-cel`, on by default). |
| `limits` | Limit counters in memory, flushed to PostgreSQL and read back about once a second. Every instance keeps a copy of every counter of a running window, loaded at start and kept current by that read, so a gateway that has not served a subject yet still knows its usage. The management API reads the stored totals. Without a database (`LimitEngine::local`, `run_local`) the counters are this instance's alone and are folded and pruned in memory. |
| `cost` | Cost of a call from its usage and a price version: tokens per million (audio and image tokens apart where the price has their rate), and units per character, image, second of audio, search unit, tool call and request. |
| `upstream` | The pooled HTTPS client to providers. It resolves names itself and refuses addresses the egress policy does not allow. |
| `relay` | The response body that hands each provider chunk to the client and a copy to the usage tap and to the response cache's capture; the timeline behind the gateway-delay figures; the recorder that settles every call exactly once, and hands the settled event and audit record to a `CallSink` when one is set (the minimal gateway's log); the audit capture (`audit.rs`) that, in audit mode, keeps the four views of a call. |
| `plugins` | Plugins ([plugin concept](plugin-concept.md)): the registry of the plugins compiled into this binary (`register` adds one, for tests), the bindings a snapshot compiles for each workspace (`bind_workspace`: the workspace's own binding, else its organisation's unless that is locked, else the platform's, the install's `default_binding`; a plugin the platform did not install is left out), the hook that runs them on a call (`run.rs`), the response cache on a call (`answers.rs`: the lookup, the capture of an answer and its write at settlement), and the host that serves their model calls as internal calls of the workspace, and their decision calls to TypeSafe's System One API (family `typesafe`, model kind `decision`), each recorded as a usage event with `parent_id` (`host.rs`); the answer's usage is read as an application's call to System One is (`extract::Dialect::SystemOne`). `mgmt::ensure_plugins` installs the plugins marked `installed_by_default` once. |
| `answers` | Where the response cache keeps answers: `valkey.rs` (shared by every instance; bounded operations and a breaker; TLS with `rediss://` and `PGW_VALKEY_CA_FILE`) wrapped in `sealed.rs` (each answer sealed with AES-256-GCM under a key derived from the current KEK for its workspace and name, the name a keyed hash; what does not open is a miss), or `memory.rs` (this instance's, the oldest out first, not sealed), behind `AnswerStore`, which also gives a store's figures, forgets answers by workspace and connects at start. `forget_cleared` frees a memory store of the answers a clear made misses, when the instance applies the clear. |
| `extract`, `sse` | Usage extraction per dialect; server-sent event framing. |
| `spool` | The durable event spool and its shipper. |
| `telemetry` | Prometheus metrics, traces over OTLP/HTTP, the live figures. |

## A request, step by step

**Proxy mode** (`/proxy/{provider}/…`, section 4.2):

1. Authenticate the key against the snapshot (an HMAC lookup, no database).
2. Buffer and scan the body. Read `model`, `stream`, `stream_options`. A body sent to a path
   the gateway reads usage from must be JSON (`415` otherwise). Provider tools the workspace
   does not allow are refused (`tools.rs`), without changing the body. The workspace's
   plugins look at a body the gateway reads a conversation from (`plugins/run.rs`); while one
   enforces, a key set to blind relay is refused (`403 not_allowed`).
3. Resolve the target: a template (`@name` or `x-gw-template`), or the provider's model id,
   which must belong to a repository model granted to the workspace. A call without a model
   the gateway checked (files, batches, blind relay) needs a credential of the workspace or
   its organisation; on the installation's shared credential only model lists pass (`403
   own_credential_required`).
4. With a template: write its model and parameters into the scanned body (`shape.rs`), or
   check the request against it. Content members keep their bytes.
5. Admission: the key's concurrency cap, then every limit that applies.
6. Replace the credential and send. A connection that failed before anything was sent is
   tried again, up to the template's attempts (one retry by default); every answer of the
   provider, a 429 included, is passed on.
7. Relay the response unchanged; the tap reads usage from a copy.
8. Settle: usage, cost, limit counters, metrics, the usage event.

A `GET` that asks to open a WebSocket (`Connection: Upgrade`, `Upgrade: websocket`) takes
another way (`websocket.rs`). The key may come in `Authorization` or as the subprotocol
`openai-insecure-api-key.<key>`, which browsers use and which is never forwarded. The session
is refused when the key requires usage (a relayed session reports none), when the workspace
enforces a plugin or restricts provider tools, on Vertex AI, and on the shared credential
without a `?model=` the gateway checks. It is admitted like a call (one request, the key's
concurrency slot held while it lasts), the handshake goes to the provider on an HTTP/1.1
connection of its own through the egress policy, with the provider's credential and the
caller's `Sec-WebSocket-Key`, and the provider's 101 is the caller's. Bytes then pass both
ways until an end closes, five minutes without a byte, an hour, or the instance stops: the
open sessions are closed and waited for before the spool stops (`Gw::sessions`). One event per
session, with its bytes and why it ended. A relayed session is not read, so audit mode cannot
record it: where the workspace records Realtime sessions and refuses those it cannot
(`audit.unrecordable`), `audit::refuses_unrecordable` refuses it before the handshake, for a
session audit mode applies to (its sample left out). Proxy mode refuses to open a Realtime
session at the provider (`opens_session_elsewhere`: an ephemeral key of OpenAI's Realtime API
or Gemini's Live API, a WebRTC call) on the shared credential, since the session reaches the
provider without the gateway, and on any credential where `refuses_unrecordable` says so.

Managed mode's `GET /v1/realtime?model=…` (`realtime.rs`) ends the WebSocket on both sides
instead. It resolves a model of kind `realtime` at an OpenAI-shaped provider (a template's
other models are passed over), admits the session like a call, and opens the provider's side
itself: a handshake of its own with the provider's credential, a key of its own and no
extensions, moving on to another model of a template as its failover policy allows until a
provider has switched. It then answers the caller's handshake with the caller's key and the
subprotocol `realtime` where the caller offered it. Frames pass unchanged both ways; the
gateway reads each event's `type` from its top-level members only (audio frames are large).
A caller's `response.create` is refused with an `error` event while a limit of tokens or spend
has nothing left (`LimitEngine::exhausted`, which counts nothing), and a `session.update`
that names provider tools the workspace does not allow, or another model, likewise; neither
reaches the provider. Each `response.done` settles one event, through a recorder made from
the session's: the price in effect when the session opened, usage from `response.usage`
(cached tokens as cache reads, audio and image tokens among the uncached input), and the
limits' current windows, since a session can outlast a window. After a settlement that leaves
a limit with nothing, the gateway closes with 1008. A caller that leaves mid-response has the
response cancelled and waited for up to five seconds. The handshake's request id names the
first response's event; a session without a response writes one event with no usage. In audit
mode, where the workspace's `audit.realtime` is set, `begin_audit` captures the handshakes and
the relay shows every frame to the ledger with the way it went (`relay::audit::Way`): passed
on or refused, or sent by the gateway itself. A `SessionCapture` holds the frames since the
last response as the four views, one line each, and each settlement cuts them into that
response's record; frames after the last response are the record of one more event without
usage.

**Managed mode** (section 4.3) serves three interfaces with one pipeline: `/v1/chat/completions`
(OpenAI-compatible), `/v1/messages` (Anthropic-compatible) and `/gw/v1/chat` (native). It
differs from proxy mode in steps 2 to 7: gateway parameters also come from the body's
`gateway` object; the caller's own parameters pass the capability filter; `429`, `5xx` and
connection errors are retried before the first byte. The workspace's plugins run between
reading the body and planning (`before_plan`), once per call: an enforcing binding's verdict
can refuse the call, route it (`RouteMatch.intent`, CEL `intent`) and count it against
intent limits (`LimitFilter.intents`); a shadow binding answers alongside the provider call,
and only the writing of the usage event waits for it. Calls a plugin makes itself carry
`Caller.internal`, which no request can set, and are never looked at. Once the call is
planned and its audit record begun, the response cache (`plugins/answers.rs`, plugin concept
section 15) looks for a kept answer to the same request; a hit is answered there, in the
caller's dialect, before the call takes a slot or counts against a limit, and a miss has its
answer captured as it is relayed and kept when the call settles. For a template with several models,
routing orders them once (`resolve_routed`), and `run` in `managed.rs` tries them in turn:
limits are admitted per model (`admit_limits`, the key's concurrency slot once), the body
is shaped again for each model's dialect, and a failure the scheme names moves the call on
before the first byte reaches the caller. The interface and the model's provider
decide the route:

| Caller speaks | Model is at | Route |
| --- | --- | --- |
| OpenAI dialect | an OpenAI-compatible provider (OpenAI, DeepSeek, GLM, Kimi, Ollama, Gemini's compatible endpoint) | Same dialect: the caller's bytes are patched. The answer gets the cost inside `usage`; a stream is shaped event by event. |
| Anthropic dialect | Anthropic | Same dialect: `system`, `tools` and `messages` reach Anthropic as the bytes the caller sent, and the answer comes back as Anthropic wrote it. The cost is in a header. |
| OpenAI's Responses API (`/v1/responses`) | a provider whose quirk `responses_api` is set (the `openai` and `xai` presets) | Same dialect: the caller's bytes are patched, with `store: false` always; a stored conversation is sent again in `input` ("Stored responses" below). The cost goes inside `usage`, and into the `usage` of the stream's last event. |
| anything else, always the native interface, and always Gemini's own API | any provider | Translated: client half of the caller's dialect → neutral model → provider half of the provider's dialect, and back. Deterministic: the same provider answer gives the same bytes, apart from what names the call itself (see "Translated answers" below). |

`/v1/messages/count_tokens`, `/v1/audio/speech`, `/gw/v1/speech`, `/v1/systemone` and the
image and speech-to-text endpoints (`/v1/images/…`, `/v1/audio/transcriptions`,
`/v1/audio/translations`) use the same resolution, admission and sending. An image edit or a
transcription is a form (`multipart/form-data`): it is read for its
model and parts (`pacod_gw_dialects::multipart`) and sent to an OpenAI-shaped provider with
only its `model` part changed, boundary and all.
`/v1/systemone` serves decision models at a TypeSafe provider only; its usage event has the
interface `decision`, as a plugin's call to System One has, but no `parent_id`. Chat refuses
decision models, and model lists leave them out.

**Stored responses** (`state.rs`) are kept in a workspace whose setting `responses.store`
is on, unless the audit rule `forbid` applies. A Responses call there keeps its response unless
it sends `store: false`, as at OpenAI. The provider keeps nothing:

1. A call that names `previous_response_id` is expanded before the plugins and planning: the
   stored turns' input and output items, oldest first, then its own input. From there it is a
   stateless call. Routing, limits, the response cache's key, shaping, translation and
   failover see the expanded body; the audit record receives the caller's own. `instructions`
   and `tools` are not inherited, as at OpenAI.
2. The gateway mints the response's id, `resp_<request id>`, because provider ids are not
   unique (a cache hit repeats one, Gemini's may be empty). Translated answers derive their
   item ids from it; every answer, and every stream event that carries the response, is
   stamped with it, `store` and `previous_response_id`.
3. The turn is written before the caller can name it. A whole answer is returned after the
   write; a stream's last event waits behind a gate on the recorder, which `RelayBody` polls
   before it reads on. A write that fails, or takes more than 10 seconds, leaves the answer
   with `store: false` (`gw_stored_responses_total{outcome="failed"}`).
4. A row of `stored_responses` holds the turn's own input items and the response as the caller
   received it, linked to the turn before. A chain is read within the workspace, only while
   every turn of it is kept, and up to the request body limit; otherwise the call is refused
   (`400 previous_response_not_found`). The retention, `responses.retention_days` (30 days
   unless set), is fixed when a row is written. The admin role's upkeep deletes expired rows;
   an erasure deletes a user's, and deleting a workspace its own. A caller reads a kept
   response, lists its input items (with ids derived from the response's where it sent none)
   and deletes it at the paths under `/v1/responses/`.

The minimal gateway keeps none (`ObjectStore::keeps_responses` is false for its record).

**Anthropic's Files and Message Batches** (section 8.3, `objects.rs`) are served at
`/v1/files` and `/v1/messages/batches` to callers that send `anthropic-version`, on the
credential a message of the workspace would use. A batch goes to the provider of its items'
models; files to the workspace's Anthropic provider, or the one `x-gw-provider` names when
it has several. Each file and batch is recorded with its
workspace and the credential that created it (`provider_objects`); lists show the caller's
own, and another workspace's id answers `404`. A message that names a file id is checked
against that record in either mode (`files.rs`): another workspace's file is refused, and so
is an unknown one on the shared credential. Keys are read as the provider reads them, escapes
decoded and every value of a repeated key counted. Apart from the file and batch endpoints,
which read and write their records, this owner lookup and the read of a stored response's
turns are the request path's only database reads, each made only for a body that names one. Each batch item is resolved, merged and
filtered like a single message and counts as a request when the batch is created. The
admin role's upkeep task polls open batches; once one has ended it reads the results and
writes one usage event per item (trace id: the batch id) at the `batch` variant of the price
in effect at creation, and the token and spend limits count that usage then. Plugins do not
see a batch's requests yet, so a workspace with an enforcing plugin refuses batches. In audit
mode an item is recorded with its event (`Gw::record_event` writes the record first): its
request is kept from the batch's creation (OpenAI: its file's upload) in `audit_batch_items`,
under the request id of that call (`ItemAudit`), and settlement reads the results in groups,
fetching their requests at most 32 MiB at a time (`settle_lines`). The record's id is the
item's event's, so an item settled twice is stored once. Once the requests are kept, the
record of the call that brought them keeps only what the batch was (`batch_summary`,
`batch_form_summary`), so that an item's request is in its own record alone, under its end
user, and an erasure finds it.

## Audit mode, prices, monitoring

**Audit mode** (section 14.6). `relay/audit.rs` decides per call whether it is recorded:
the platform's and the organisation's rule, the workspace's settings, the template's
switch, and what the call asked for. A recorded call keeps four views: the caller's request,
the provider request of every attempt, the provider's answers, and what the caller received.
A body that equals another is stored once and referred to. Attachments in full mode (long
base64 payloads in the bodies) are stored once per workspace and content in
`audit_attachments`, referred to by hash, and put back when the record is read; erasing
records removes the attachments no remaining record refers to. Credentials are removed before
anything is kept. The record is written when the call settles, into a spool of its own, and
the usage event of the call names it. `GET /requests/{id}/audit` returns it; `{id}` is the
event's id or the value of `x-gw-request-id`. A Realtime session is recorded per response
(see above); its bodies are JSON lines, referenced frame by frame in reference mode, the audio
of `input_audio_buffer.append`, of `input_audio` parts and of audio deltas whatever its length.
The calls about files and batches (`objects.rs`, `openai_objects.rs`) begin their record in
`begin_as` where `audit.objects` is set. An upload that streams through is kept by a
`RequestTap` as it passes; a download streams through `RelayBody`, so that its record holds
all of it and the call settles at its end. In reference mode a form keeps its text parts and,
of each file, what it is (`form_summary`), and a file's content its size and hash.

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
in memory (`telemetry/live.rs`). Every instance writes them, as raw counts, into its row of
the instance registry every five seconds (`cluster/report.rs`); the management API adds
up the rows of the running instances, or shows one (`/monitoring/live?instance=`), and uses
its own figures for itself. Open alerts are computed on request (`/monitoring/alerts`):
limits (from the stored counters), prices, proposals, providers, and per instance spools,
configuration lag, a stopped heartbeat, an id used twice, differing secrets. The alert
rules for Prometheus in `deploy/prometheus/rules/` read the exported series, among them
`gw_limit_used_ratio` (exported by management instances only), `gw_price_age_days` and
`gw_upstream_health`.

## What is stored where

| Data | Store |
| --- | --- |
| Configuration, its versions and the audit log; job leases and runs | PostgreSQL |
| People, role bindings, admin tokens, invitations, console sessions | `PGW_AUTH=local`: PostgreSQL, database `pacod_gw`. `PGW_AUTH=oidc`: pacod-id, database `pacod_id` in the same server by default; the core keeps a copy of the bindings in memory |
| Organisations and workspaces on their way to pacod-id | PostgreSQL (`idp_outbox`) |
| Running instances, their applied version and live figures (`instances`) | PostgreSQL |
| Limit counters | Memory, synchronised through PostgreSQL |
| Answers of the response cache | Valkey (`gw-valkey`, memory only, nothing on disk), or each instance's memory without `PGW_VALKEY_URL` |
| Usage events and audit records | Spool on each instance's disk, then ClickHouse |
| Files and message batches created at Anthropic through the gateway, with their workspace | PostgreSQL (`provider_objects`) |
| Responses of the Responses API a workspace keeps, with their input items, until their retention ends | PostgreSQL (`stored_responses`) |
| Instance id | `PGW_INSTANCE_ID`, or `instance.json` in the instance's spool directory |
| Provider credentials | PostgreSQL, sealed with a data key wrapped by the key-encryption key from the environment |
| API keys and admin tokens | PostgreSQL, as HMAC-SHA256 with a pepper from the environment; the secret is shown once |

The minimal gateway stores one thing: its record of files and batches, in `PGW_OBJECTS_DIR`
when set (an append-only `objects.jsonl`, compacted at start, under a lock that keeps a
second process off it). Its configuration, provider keys and client keys are environment
variables (`deploy/lite/.env`); in memory the provider keys are sealed under a key-encryption
key and the client keys hashed with a pepper, both made new at every start. Limit counters
and the response cache's answers live in memory only, and calls go to the process's log.

## Where the code differs from the concept

The concept is the design; this is what was built differently, or not built. Section
numbers are the concept's.

| Area | In the code |
| --- | --- |
| Configuration tables (appendix A) | One table of typed JSON documents (`config_objects`) with a kind, a parent and a name, instead of one table per kind. The types are in `model/`. Template versions are kept twice: the template's document holds its 50 most recent, which the request path needs; `template_versions` holds every one, and older versions are read from there. Discovery proposals have a table of their own (`discovery_proposals`); discovery runs are job runs. |
| Grants (7) | A workspace without grants of its own uses its organisation's. The built-in `_system` organisation may use every active or deprecated model without grants. A retired model's successor is used where the grant has `follow_successor`; the successor needs no grant of its own, and a key's allow-list that names the retired model allows the successor. |
| Proxy mode and the repository (4.2) | A call that names a model is served only if that model is in the repository and granted to the workspace. A call without a model the gateway checked (files, batches, a blind relay, Ollama's housekeeping) is relayed only on a credential of the workspace or its organisation, never on the installation's shared one, where it would reach other workspaces' objects; a provider's model list passes on any credential. A model call must be sent as JSON. A workspace that restricts provider tools has no blind relay. |
| `x-gw-cost` (8.1) | In the price's currency, as the request log shows it; reports and spend limits use the reporting currency. Sent on non-streaming responses of managed mode, speech included. Proxy mode relays the answer while it arrives and learns the usage at its end; the cost is in the request log under the request id. |
| OpenAI Files and Batches (8.3) | `/v1/files` without `anthropic-version`, and `/v1/batches`, at a provider whose quirk `openai_objects` is set (`openai_objects.rs`). A file of purpose `batch` is read at upload: each line is planned like a single call at its endpoint (chat completions, Responses, embeddings), must be served same-dialect by the file's provider, and is written with the provider's model id; the decisions per line are kept with the file (`detail.batch`), and a batch is made only from such a file. The file is not read again later, because OpenAI does not let every purpose's content be downloaded. Limits are admitted per line when the batch is made. Output and error files are recorded as the batch's workspace's when the batch is read or polled. Settlement reads both files and writes one event per line at the batch rate. Not served: `/v1/uploads`, deleting a batch, ascending lists. A vector store or container named in a call needs an own credential. |
| Anthropic Message Batches and Files (8.3) | A file id the gateway has no record of passes on a workspace's or organisation's own credential, where it may have been uploaded directly, and is refused on the shared one. A blind relay is not read, so its file ids are not checked; it runs only on such an own credential. Batch items get no failover: all of them go to one Anthropic provider, and an item whose model (for a template, its first choice) is elsewhere is refused. A batch's `results_url` names the gateway's own path. A batch can be deleted only once it is accounted (`409 batch_not_settled`). These endpoints are recorded in audit mode only where `audit.objects` is set. A batch made in proxy mode, whose items the provider runs unseen, cannot be recorded: it is relayed unless `audit.unrecordable` is `refuse` (`refuses_unrecordable` with `Unrecordable::Batch`). A batch's requests are written to PostgreSQL before it goes to the provider, a step on the request path that waits for storage, which concept 14.6 rules out: its items could not otherwise be recorded with their requests. These endpoints do not take a slot of the key's concurrency cap, and answer with `x-gw-request-id` only, not the other `x-gw-*` headers. |
| `usage_policy` (4.2) | A key without a policy of its own is automatic: `require` while an enforced token or spend limit counts its calls (one of its workspace or organisation not aimed at other keys, or of its rate class), `estimate` otherwise. Keys created before this rule keep the policy stored with them. |
| Rollup tables (14.4) | Hourly totals by organisation, workspace, key, provider, model, template and status, and daily totals by end user, rebuilt from the deduplicated events by a leased job in the analytics service (`jobs/rollups.rs`) every five minutes: the last two closed hours, hours that ingest marked late, and hours never built (those of the last two days, and older ones once, walking back a week at a time). The overview, the breakdown by those columns and the end-user list read them for built hours or days that are not marked late, and the events for the rest, so that their answers are the same as from the events. The usage report reads the events, because its points carry exact delay percentiles; so do breakdowns by tag, whose group of calls without the tag a tag rollup could not give exactly. |
| Errors in reports (14.4) | As the concept defines them: every status other than `ok`, which includes calls the gateway refused (`rejected`) and calls the client cancelled. The Prometheus alert on the error rate leaves cancelled calls out. |
| Audit record (14.6) | `resolution` names the template, its version, what was dropped and adjusted, the limits checked, the credential by id and fingerprint, and in `parameters` where each parameter's value came from (`caller`, `template_default`, `template_locked`, `template_omit`, `ceiling`, `model_minimum`, `fitted`, `filter`). For an attempt that was retried, the record keeps the provider's status and headers, not its body. The size limit (`audit_max_body_bytes`) applies to each body, not to the record as a whole, and chunk timings stop after 10,000 chunks. A record's expiry is fixed when it is written: a later change of `retention_days` applies to new records. In proxy mode the record does not show the routing (candidates, models passed over and tried), and after a failover its model and credential are those of the first model. Listing records (without their bodies) is not written to the audit log; reading, exporting and deleting them is. |
| Metric names (14.7) | `gw_cost_micros_total` (millionths of the currency, so the counter is an integer) and `gw_config_apply_lag_versions` (versions behind, not seconds). |
| Translated answers (8.5) | Byte-stable for the same provider answer, except what belongs to the call: `created` in OpenAI-shaped answers (the gateway's clock), and on the native interface the answer's `id` (the gateway's request id) and the `gateway` block's `request_id` and `latency_ms`. |
| Roles (5.4) | An organisation admin changes everything on the organisation (display name, settings, locks, audit rule, routing assignment) except its name and whether it is enabled, which stay with platform admins, and its caps (organisation limits), which platform admins set and reset. A workspace admin may rename their workspace and switch it off. Anyone with a role in a workspace may read which models its organisation has. The audit log is read by platform admins and auditors and by organisation admins and auditors, not by workspace auditors. Credentials, fingerprints included, are listed only to the admins who may manage them, not to viewers and auditors. Deleting a user, an organisation or a workspace removes the role bindings that name it. |
| Members (5.4, 10) | `orgs/{org}/members` and `orgs/{org}/workspaces/{ws}/members` list members with their roles and change or remove them; people are added by invitation (`orgs/{org}/invitations`), which is stored outside the configuration in its own table. Role bindings remain the underlying resource. With `PGW_AUTH=oidc` members and invitations are pacod-id's, at the scopes `organisation:<org>` and `gw.workspace:<ws>`, and accepted on its pages. |
| Deployment setups (17) | A fourth setup besides the concept's minimal (core and PostgreSQL, no analytics), standard and scaled: the minimal gateway, `pacod-gw-lite` ([lite.md](lite.md)), without PostgreSQL. It does not bring back the catalogue's `single` mode on another store: it builds the same snapshot from environment variables and leaves out what needs storage (management API, console, reports, audit storage, shared limits). Files and batches are served with a record of its own, an append-only file in `PGW_OBJECTS_DIR` (`pacod-gw-lite/src/objects.rs`, an `ObjectStore`), and its batches are settled by a task of its own every 10 seconds. Its provider keys count as the operator's own credential, and its pools are templates with one routing scheme each, without rules. The intent plugin is one binding of its one workspace, with the catalogue from a file (`PGW_INTENT_*`); without routing rules and limit filters it blocks and flags only. With `PGW_LOG_BODIES=off` its server's sink drops the audit record a flag captures, so a flag does not log bodies then; it does not set the global audit rule `forbid`, which would switch the response cache off too. |
| Several instances (11.2, 12.4, 17) | A key's `max_concurrency` is shared out over the running gateways (`ceil(max / N)` each) instead of holding per instance. The console's monitoring adds up all instances from figures they publish in PostgreSQL; the management API never calls a gateway. A heartbeat takes over an instance's row only from the same process, a restart of the same spool directory, or a stopped process. Job progress on the event stream (price and discovery runs) reaches only the streams of the management instance that ran the job; the console reloads a page on it, so a console behind another instance shows a finished run only when the page is reloaded. The upkeep round (which runs discovery) keeps its lease while it lasts, so no other instance starts one beside it; a discovery run still open when a round begins was left by an instance that stopped, and is closed as failed. The console's alert that an instance is behind the configuration is raised at any lag; the Prometheus rule waits 30 seconds. |
| Routing (7.5) | Breakers, round-robin turns and latency averages are per instance; only managed calls feed them. Proxy mode routes among the pool's models at the URL's provider and fails over among them as the scheme allows, before the first byte and only for requests it reads (not a blind relay); its calls do not feed the breakers, a refusal by a limit ends a proxy call instead of moving it on, and a scheme's `deadline_ms` is not applied. Token counting, speech and embeddings route and fail over like chat, passing over models that cannot serve them, without `deadline_ms` either; Gemini's embedding batches move on only before the first batch. Prices for `cheapest` are those in effect when the snapshot was built. The input size for `input_tokens` and the context-window check is the body size divided by four. |
| Interfaces (8.3, 9.5) | An `anthropic-beta` header reaches Anthropic models only; for a model of another provider it is dropped without a note in `dropped_params`, whatever `unsupported_params` says. `provider_options.anthropic` is read on the native interface only. Proxy mode answers its own errors in the envelope of the path's API: Google's on Gemini's own API, Ollama's (`{"error": "…"}`) on its `/api/` paths, OpenAI's on the OpenAI-compatible paths of both (`/v1beta/openai/`, `/v1/`). `x-gw-prompt-cache` is not read in proxy mode; there the template's `prompt_cache` decides. |
| Gemini interface (8.2, 9.1) | The request log and the audit record name the path as a template (`/v1beta/models/{model}:generateContent`, and the method's for counting and embeddings), with the interface `gemini`. A gateway key in `?key=` is taken out of the URI before anything reads it. A stream without `alt=sse` is one JSON array (`application/json`), closed with `]` also when the stream breaks. Only model methods are served: files, context caches, tuned models and Vertex AI's other resources answer Google's 404 and point to proxy mode, and the SDK's Vertex AI mode lists no models (Gemini API mode does, at `/v1beta/models`). In Vertex AI mode a name the SDK split (`openai/gpt` → `publishers/openai/models/gpt`) is read back; `publishers/google/models/x` is the gateway's `google/x` where such a model exists, else `x`. A model named in the body (`generateContentRequest.model`, a batch's `requests[].model`) must be the path's. Embeddings take text only; a content's text parts are one text. The same-dialect route serves Gemini's API only, and only while the conversation holds no signature the gateway wrapped; Vertex AI is always translated (until a recording shows whether Gemini's API and Vertex AI accept each other's signatures). |
| Vertex AI (8.1) | Not recorded yet: the defaults rest on Google's reference. A service account's token is cached per credential in each instance (refreshed five minutes before it expires, one request at a time, a failure remembered for 30 seconds) and fetched again once after a 401 on managed calls; proxy mode only forgets it. The token endpoint is fixed; `PGW_GOOGLE_TOKEN_URL` overrides it for tests only. Proxy mode is pinned to POST, the provider's own project and location (or the express path), the model methods `generateContent`, `streamGenerateContent`, `countTokens`, `embedContent`, `batchEmbedContents` and `predict`, no `%`, `..` or `//` in the path and no query but `alt=sse`; Google's headers and method overrides of the caller are removed. The probe is `countTokens`; discovery answers 409, and discovery runs pass Vertex providers over. `trafficType` maps `ON_DEMAND` to the standard price, `ON_DEMAND_PRIORITY` to `priority`, `ON_DEMAND_FLEX` to `flex` and `PROVISIONED_THROUGHPUT` to `provisioned` (documentation only). |
| Responses API (9.1, ING-05) | Stateless at every provider. A workspace whose setting `responses.store` is on keeps responses itself and replays them ("Stored responses" above): such a call reads PostgreSQL on the request path when it names `previous_response_id`, against section 1.3(4), and its answer waits for the write, against 14.6. Elsewhere `store: true` and `previous_response_id` are refused with `400`, the reason naming the setting. `conversation`, `background: true`, a stored `prompt` and `item_reference` items are refused with `400`, whichever provider serves the call. Where the record keeps responses, `GET` and `DELETE /v1/responses/{id}` and `GET /v1/responses/{id}/input_items` serve the workspace's own (another workspace's id answers `404`), whatever the setting says now, so that what was kept can still be read and deleted; cancelling answers `400`, since nothing runs in the background, and every other path under `/v1/responses/` answers `404 not_stored`, as all of them do in the minimal gateway. These calls are in the request log under the interface `responses`; audit mode does not record them. On the same-dialect route `store` is set to `false` (OpenAI keeps responses by default, and a credential several workspaces share would let each read the others'), `include` gains `reasoning.encrypted_content` for reasoning models so that reasoning crosses turns without a stored response, and reasoning items another family wrote (`encrypted_content` starting `gw1.`) are left out of `input`. Translated, the answer's reasoning carries its family in `encrypted_content` (`gw1.anthropic.<signature>`, Gemini's labelled as it is), OpenAI's stays unlabelled; built-in tools and their items (web search, file search, code interpreter, computer use, MCP) are refused as `unsupported_feature`; a file by `file_id` and audio input are refused. Item ids derive from the response id (`msg_<id>_<n>`), each text block is a message item of its own, and the answer repeats `instructions`, `tools`, `text`, `reasoning` and the sampling members as the caller sent them. Proxy mode on the shared credential refuses the members that reuse stored state (`403 own_credential_required`); `store` itself is relayed. An OpenAI or xAI provider made before this release has no `responses_api`: its Responses calls are translated to chat completions until it is set. Chat callers are never translated to the Responses API. |
| Images (9.1, ING-10) | Same dialect to OpenAI-shaped providers: a generation is patched like any JSON body, an edit keeps every byte of its form but the model's. Translated to Gemini's own API and Vertex AI: models whose id starts with `imagen` through `:predict` (all images in one call, priced per image; edits refused), the others through `:generateContent` with `responseModalities` `["TEXT", "IMAGE"]`, one call per image, priced by their tokens. Translated answers are base64 only (`response_format: url` is refused: the gateway keeps no images), and a mask is refused. Sizes map to aspect ratios (1024x1024 to 1:1, 1536x1024 and 1792x1024 to 16:9, the portrait sizes to 9:16); others are refused. Without usage from the provider (DALL·E, Imagen) a call is priced per image; the price variant is the first of `<quality>-<size>`, `<quality>` and `<size>` the price has. A form is limited by `max_body_bytes` (32 MiB by default) and to 64 parts; the audit record keeps its text parts and, of each file, its name, type and size. In proxy mode an edit is relayed unread, so it needs a credential of the caller's own on a provider whose credential is shared, like any call without a model the gateway checked. |
| Speech to text (9.1, ING-10) | Same dialect to OpenAI-shaped providers, the form kept but for its model. The cost is what the provider reports, never an estimate from the audio: tokens where the answer carries them (gpt-4o-transcribe, Gemini; audio tokens apart where the price has `input_audio`), else seconds (`usage.seconds`, `verbose_json`'s `duration`, or the end of the last cue of an `srt` or `vtt` answer, a little short of the audio). A model priced by the second whose caller asks for `json` or `text` is asked for `verbose_json`, the only format every provider reports a duration in, and the caller's format is written from it (`json` as `{text, usage: {type: duration, seconds}}`, `text` with a line end after it, as OpenAI's reference shows; neither checked against a recorded answer); a model priced by tokens whose caller asks for `text` is asked for `json`. Translated to Gemini's own API, the audio goes inline (WAV, MP3, AIFF, AAC, OGG, FLAC; at most 20 MB, else `413`) with an instruction to transcribe, or to translate into English; `srt`, `vtt`, `verbose_json`, timestamps and streaming are refused. A stream (`stream=true`) is relayed, and its `transcript.text.done` carries the usage. |
| Rerank (9.1, ING-10) | Callers speak Cohere's shape (`query`, `documents` as strings or `{text}`, `top_n`, `return_documents`) at `/v1/rerank`, and at `/v2/rerank`, where Cohere's SDK calls. Every call is written for its provider's API and its answer written back in Cohere's shape: Cohere's `v2/rerank` (strings only, no documents returned: the gateway fills them in from the request), Jina's `rerank` (also vLLM and Infinity) and Voyage's (`top_k`, `data`), chosen by the family `cohere` or the quirk `rerank_api`. There is no same-dialect route. Cohere is priced per search unit, the others by tokens; without the provider's count the call is estimated, one search unit per 100 documents where the price has a search-unit rate, else characters divided by four. The cost is in `meta.cost`. |
| Realtime (9.1, ING-11) | Managed sessions are recorded in audit mode only where `audit.realtime` is set, also under an organisation's `force`. WebSocket sessions relayed in proxy mode, and Realtime sessions an ephemeral key or WebRTC opens at the provider, cannot be recorded: they are served without a record unless `audit.unrecordable` is `refuse`. Managed sessions are not shown to plugins (a workspace that enforces one refuses them), and not cached. Input audio transcription that a session switches on is reported by the provider on `conversation.item.input_audio_transcription.completed`, for another model, and is not accounted. Cached audio tokens are priced at the price's `cache_read` rate, which OpenAI's realtime prices share with cached text. The session's idle (five minutes without an event; the gateway's pings every 30 seconds and their pongs do not count) and longest (an hour) times are fixed, as in proxy mode. Ephemeral keys (`/v1/realtime/client_secrets`, `/sessions`) and SIP calls are not served: a browser connects with its gateway key as the subprotocol `openai-insecure-api-key.<key>`. Responses are not metrics or live figures of their own beyond the request log's events. |
| Embeddings (9.1) | `POST /v1/embeddings` takes text, not token arrays (those go to OpenAI's own models only, through proxy mode), at most 2,048 inputs. To Gemini's API it is `batchEmbedContents` in calls of 100, to Vertex AI `predict`; the vectors come back as OpenAI writes them, base64 of little-endian float32 when asked. A provider that reports no usage (Gemini's API never does, recorded) is booked by an estimate of the input, its characters divided by four, never as output; proxy-mode embedding calls likewise. An OpenAI answer larger than the 8 MiB the usage tap holds has its usage read from its end. `provider_options.gemini` (`taskType`, `title`) applies to every input. |
| Providers (8.1) | Kimi, Ollama, xAI and Vertex AI are verified against their documentation and the mock's emulations only; DeepSeek, GLM and Gemini against recorded answers and live calls (section 8.6), and the Gemini interface with Google's Python SDK (`google-genai` 2.29) in both its modes against the live providers. Gemini's speech models (`-tts`) are proposed with the kind `speech`, but managed speech serves the OpenAI wire only. Gemini's own API takes images inline only: an image URL is refused. A function call without an id gets one made up (`gm_<response>_<n>`), which is never sent back to Gemini. GLM cannot take `tool_choice: none`: it is dropped and reported, not imitated by leaving the tools out. |
| Quirk profiles (8.2) | A provider's quirks (`drop_fields`, `request_fields`, `normalise_history`, the reasoning and usage fields, `echo_reasoning`, `thought_signatures`) and a model's own `drop_fields` apply to the OpenAI-compatible wire only. A provider on Anthropic's API or on Gemini's own API ignores them; Gemini's own options come per request in `provider_options.gemini`. `provider_options` never set a member the gateway governs (limits, sampling, reasoning, the conversation): it is left out and named in `dropped_params`. The reasoning tokens a provider reports outside `completion_tokens` (xAI, Gemini's compatible endpoint) are added to the output: the larger of the reported count and the part of `total_tokens` the other counts leave. A model whose capability `reasoning_with_tools` is off (GPT-5.6) gets `reasoning_effort: "none"` on a managed request that offers tools, translated or not; an effort asked for is replaced and reported in `adjusted_params`. |
| Same-dialect bytes (8.3, 8.5) | The conversation keeps its bytes on the same-dialect route, with three exceptions, each applied only when its case is present: the history normalisation a provider's quirks ask for; a provider error whose body is not shaped as the dialect's (Gemini's array, Ollama's string) is rewritten into the OpenAI envelope with the provider's status; and reasoning another provider produced is removed before Claude would refuse it (`thinking` blocks with an empty or `gw1.`-prefixed signature, reported in `dropped_params`). OpenAI accepts other providers' fields in the history (checked live), so nothing is removed on the OpenAI route. On the Gemini route, `generation_config` and snake_case members of `generationConfig` become one camelCase `generationConfig` (where both name a member the camelCase one is kept), so that what the gateway governs is what Gemini reads; proxy-mode templates on Gemini read it the same way. Vertex AI's `labels`, `routingConfig` and `modelArmorConfig` are removed and reported, a `model` member is removed (the path names the model), and a reasoning level is kept in the caller's case. |
| Prices (13.1) | Tiers (`above_input_tokens`) and windows (variant by weekday and UTC hours) are read from LiteLLM's list (`*_above_<N>k_tokens`, `off_peak_pricing`), from a pricing page, entered by hand or imported. Extraction from a page needs a capable model: a small one (gpt-4.1-nano, tried live) keeps the structure but mixes up rates. Holiday calendars: a holiday is a date in the calendar's time zone; make-up working days are not listed and keep their weekday's windows (DeepSeek's wording puts its off-peak rates on weekends without exception; not confirmed with DeepSeek). A provider made before the DeepSeek preset named `cn` needs `holidays: cn` set by hand. A feed's `key_prefix` names the prefix LiteLLM files a provider's models under where it differs from the gateway's name (`zai`, `moonshot`, `gemini`, `deepseek`); `models/` is ignored when matching. |
| Limits (12.1) | Organisation limits count in UTC windows; workspace limits in the workspace's time zone. |
| Model lifecycle (6.3, 6.4) | `skip_probe` lets a platform admin activate a model without a successful probe. Probes and discovery call the provider with the shared credential only, while the activation gate also accepts an organisation's or workspace's credential: a provider with only such credentials needs `skip_probe`. Discovery proposals show the capability preset and the kind the gateway suggests; accepting can change both, and an explicitly chosen preset keeps its own image support. Discovery reads what a provider's list reports: the context window and output limit (Anthropic, DeepSeek, Kimi, Gemini's own list), image input and reasoning (DeepSeek, Kimi, Gemini's own list, and Ollama's `/api/show` for the first 50 models of a server); not Anthropic's capabilities tree. Accepting a proposal chooses the capability preset from the model's id and these reports, so the draft still needs a look, and names the model `<provider>/<id>`, or by the provider's id alone with `naming: bare` (as some applications name Gemini models). A local Ollama model is accepted as free; one on ollama.com is not. Asking for a discovery run is not written to the audit log. |
| Management API (10.1, 10.2) | Some resources of 10.2 live elsewhere: capability profiles are presets (`/capability-presets`) copied into each model; price history is `/models/{id}/prices`; price sources are part of the price job; an organisation's or workspace's credentials are `/providers/{id}/credentials` with a scope, and `/providers/{id}/credentials/{cid}/copy` gives one a key already stored (the installation's, or for a workspace its organisation's) as a credential of its own, without entering the secret again; a workspace's settings and tag keys are fields of the workspace; limit state is `/limit-state` and end-user limits are `end-user-overrides`; CSV exports are the analytics reports' own (`?format=csv`). Sorting and filtering apply to the collections of plain objects; the other lists page by cursor only or are answered whole. `?dry_run` exists where the OpenAPI document lists it and is refused elsewhere. A member's roles change without a version. |
| Configuration export (11.5) | A template carries its 50 newest versions; older ones are not exported. An import checks each document's kind and names, not every check of a single change; an unusable object is left out of the configuration and reported. |
| Reports (14.5) | The CSV of the errors report holds the table by provider and model, the CSV of the delay report its series; their other parts are in the JSON answer only. |
| Traces (14.8) | Exported over OTLP; the shipped collector writes them to its log only, until a backend is configured. |
| Outbound connections (17) | Besides the providers, the core connects to the analytics service and, with `PGW_AUTH=local` and SMTP configured, the SMTP server for account emails. With `PGW_AUTH=oidc` it connects to pacod-id's internal listener instead and sends no mail; pacod-id sends it. |
| Sign-in (5.4) | With `PGW_AUTH=oidc` the gateway's global settings for sessions and links (`session_hours`, `remember_me_days`, `verification_link_hours`, `reset_link_minutes`, `invitation_link_days`) have no effect: the realm's settings in `deploy/pacod-id/pacod-id.toml` apply. A member `PUT` with `role: null` for someone who is not a member answers 204, not 404. Audit entries of invitations keep the invitee's address after the account is deleted. The core's account code (`mail.rs`, `account.rs`, sessions) stays for `PGW_AUTH=local`. |
| Console (15.2) | A workspace's model grants page shows each model's state, not its price. |
| Response cache (2, 9.2, 19; [plugin concept](plugin-concept.md), section 15) | Built as a plugin, installed by default, earlier than phase 2 placed it. A new installation installs it off (`PGW_CACHE_DEFAULT_MODE`); the development stack and the demo install it on. `cache.response` in the `gateway` object and the header `x-gw-cache` are read: `on`, `off` or `refresh`. Valkey is on the request path, against section 1.3 (4): a lookup waits for one read, bounded by the binding's time (100 ms), and a failing store is skipped. Managed mode only; proxy mode and embeddings are not cached. |
| Plugins ([plugin concept](plugin-concept.md)) | Bindings' settings are one JSON object checked by the plugin it names, stored as the plugin reads them. The API describes them as one of the plugins' settings, told apart by `settings.plugin` (a `oneOf`, as the concept's tagged enum would be): it shows that tag, and accepts settings with it or without, but not with another plugin's. A binding names its catalogue by `settings.catalogue`; catalogues are the intent plugin's kind, not a generic one. Rules match the label whatever its confidence unless they set `confidence`; there is no `errored` condition (an engine error never reaches the rules). Scores are scaled from 0 (the first level) to 1 (the last) for both engines. The cascade asks the LLM for the intent only and keeps Jev's flags, scores and choices. The evaluation of a labelled set is an endpoint of the core (`plugins/intent/evaluate`, at most 200 samples) with a panel on the workspace's Plugins page, not a tester suite; the engines' calls of the try and evaluate endpoints carry `task: plugin.intent` like those on the request path. Proxy-mode calls are looked at after their body is read, before the template is resolved. |

## Ports

| Service | In the container | Development stack (`make dev-up`) |
| --- | --- | --- |
| Core, data plane | 8080 | 18080 |
| Core, management API (`pacod-gw-admin` on `pacod-admin-net`) | 8081 | 18081 |
| Core, metrics | 9100 | 19100 |
| Analytics | 8090 | 18090 |
| Console (through Caddy) | 3000 | 13000 |
| Mock provider | 9000 | 19000 |
| Load agent (`local`) | 8096 | 18096 |
| Mock provider of load tests | 9000 | 19001 |
| PostgreSQL | 5432 | 15433 |
| ClickHouse HTTP | 8123 | 18123 |
| Valkey (the response cache) | 6379 | 16379 |
| Prometheus | 9090 | 19090 |
| Alertmanager | 9093 | 19093 |
| Grafana | 3000 | 13001 |
| pacod-id, sign-in pages (through Caddy; the issuer) | 8070 | 13070 |
| pacod-id, internal API | 8071 | 18071 |
| pacod-id, metrics and probes | 9170 | 19170 |
| Mailpit (pacod-id's mail in development) | 8025 | 18025 |

With the scale layout (`make dev-scale-up`) 18080 is Caddy balancing over `gw-gateway-1`
(18082) and `gw-gateway-2` (18083); a gateway that plays another machine (`make node-dev-up`)
answers on 18180, and a load agent that plays another machine (`make load-agent-dev-up`) on
18196. With `DEV_HTTPS=1` (`make dev-up DEV_HTTPS=1`) Caddy also serves
`https://console.localhost` and `https://api.localhost` on ports 80 and 443. The runbook's
section 11 has the details.

The minimal gateway listens on 8080 in its container. Its compose file publishes it on
127.0.0.1:14080 (`PGW_LITE_BIND`, `PGW_LITE_PORT`); its development stack
(`make lite-dev-up`) on 18280, with its mock provider on 19280.

The tests use their own PostgreSQL, ClickHouse and Valkey on 25433, 28123 and 26379
(`make test-services`).

## Working on it

```sh
make test-services   # PostgreSQL and ClickHouse for the tests
make verify          # format, lints, all tests, OpenAPI drift, console checks
make dev-up          # the whole stack in containers, with the mock provider
make smoke-mock      # the end-to-end flow against the mock
make smoke-live      # a few tiny calls with the real keys in .env
make load PROFILE=proxy
make load-agent-dev-up   # a second load agent, playing another machine
make deploy-check    # static checks of deploy/

cargo test -p pacod-gw-lite   # the minimal gateway, without a database
make lite-dev-up     # the minimal gateway in a container, with its mock provider
make lite-smoke      # calls through it, then each call's line in its log
```

[CLAUDE.md](../CLAUDE.md) lists the rules for changes (secrets, migrations, generated files).

[runbook.md](runbook.md) covers deployment and operations.
