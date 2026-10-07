# pacod-gw: AI proxy and gateway concept (design v0.2)

| Field | Value |
|---|---|
| Date | 2026-10-04 |
| Status | Concept, draft for review |
| Inputs | [feature catalogue](ai-gateway-feature-catalog.md) · [design assessment](ai-gateway-design-assessment.md) · [PACOD LLM usage](LLM_USAGE.md) · the current scaffold ([architecture.md](architecture.md)) |
| Decisions already taken | Tenancy is organisation → workspace. The first release covers PACOD parity plus the requirements in section 1.2, with Anthropic supported in full. MCP governance, guardrails and the capability broker follow later. |
| Not available | Design v0.1 and the market analysis cited by the catalogue are not in this repository. This concept relies on them only through the catalogue and the assessment. |
| Marker | "(to verify)" means a statement about a provider's behaviour that must be confirmed with a recorded fixture before it is built on (section 8.6). |

---

## 1. Summary

### 1.1 What it is

pacod-gw is an AI gateway with three parts: a Rust core that carries the traffic and exposes every management function as an API, a SvelteKit console that uses only that API, and an analytics and observability part that stores usage, computes statistics and runs batch jobs.

Applications reach LLM providers through the core in one of two ways. In **proxy mode** the core relays the provider's own API byte for byte and only adds authentication, limits and accounting. A workspace template can still choose the model and set its parameters there, without touching the conversation content. In **managed mode** the core understands the request: it resolves a workspace template, merges parameters, removes what the chosen model does not accept, translates to the provider's dialect and returns a normalised answer with usage and cost.

The first consumer is PACOD. The first release replaces PACOD's LiteLLM proxy and takes over the LLM management PACOD built around it: model catalogue, templates, provider keys, pricing, quotas, usage accounting, metrics and debugging.

### 1.2 Requirements and where they are answered

| # | Requirement | Section |
|---|---|---|
| 1 | Three components: Rust core, SvelteKit UI, monitoring / analytics / statistics / observability | [3](#3-architecture), [14](#14-monitoring-analytics-and-observability), [15](#15-ui) |
| 2 | Efficient, fast, lightweight core with a proxy mode that does not touch content | [4](#4-data-plane) |
| 3 | Workspaces as the main unit separating customers, applications, organisational units, projects | [5](#5-organisations-and-workspaces) |
| 4 | Global model repository with life-cycle management; workspace templates with preconfigured LLMs | [6](#6-model-repository), [7](#7-workspace-templates) |
| 5 | Providers: OpenAI, Anthropic, Google Gemini, GLM, DeepSeek, Kimi, Ollama | [8](#8-providers-and-adapters) |
| 6 | A gateway-own interface that abstracts the provider interfaces and adds gateway parameters | [9](#9-interfaces) |
| 7 | All configuration, administration, monitoring and tracking exposed as an API on the core; the UI uses only that | [10](#10-management-api), [15](#15-ui) |
| 8 | A batch process, configurable from the UI, that tracks the prices of all provisioned models | [13](#13-cost-model-and-price-tracker) |
| 9 | Models, model configuration, provider keys and the like change on the fly, without a restart | [11](#11-configuration-and-hot-reload) |
| – | Audit / debug mode per workspace or template, storing complete requests and responses | [14.6](#146-audit-mode) |
| – | The delay added by the gateway stored for every call and shown in the UI and the API | [14.2](#142-gateway-delay) |
| – | Full support for Anthropic | [8.3](#83-anthropic-in-full), [9.4](#94-the-anthropic-compatible-interface) |
| – | Cover at least the features PACOD uses | [18](#18-pacod-coverage-and-migration) |
| – | Build on the feature catalogue | [19](#19-catalogue-mapping-and-phasing) |

### 1.3 The decisions that shape the design

1. **Two processing modes**, selected by URL prefix and allowed per API key, never per request: proxy and managed. Templates work in both.
2. **The gateway-own interface is the internal canonical model.** Every managed request is converted into it, so exposing it adds no second translation layer.
3. **Two dialect families, both complete,** in the first release: OpenAI-compatible (six of the seven providers) and Anthropic Messages. Each is a client interface and a provider adapter, with translation between them in both directions.
4. **Configuration lives in PostgreSQL** and reaches the request path as an immutable in-memory snapshot that is swapped atomically. Nothing on the request path reads a database.
5. **One core binary, two listeners.** The data plane and the management API share a process by default and can be split by role.
6. **The core is the only API surface.** The analytics service is internal; the core authorises analytics queries, forces the workspace scope and forwards them.
7. **Usage events are durable.** Each call is written to a local append-only spool before it is shipped to ClickHouse.
8. **Cost is stamped when the call is settled**, from versioned prices that include cache and reasoning rates. An unpriced model is flagged, never given a default price.
9. **Tenancy has two levels plus the key.** Allow-lists intersect and numeric caps take the minimum; there is no other inheritance rule.
10. **Failure handling is conservative.** A timed-out call is never retried, retries happen only before the first response byte, and there is no fallback or load balancing unless someone configures it: a template with several models and a routing scheme (section 7.5).

---

## 2. Goals, non-goals and first users

### Goals

- Replace LiteLLM in PACOD with no loss of function and with a small, reversible change on PACOD's side (base URL and key).
- Serve several organisations and workspaces from one installation, with separate keys, templates, limits and reporting.
- Keep the footprint small: one core binary, PostgreSQL for configuration, and an optional analytics stack.
- Make every operational change (model, template, key, price, limit) effective within about a second, with an audit trail.

### Non-goals for the first release

- MCP and tool governance, content guardrails, a policy language, capability-based model discovery and ranking.
- Response caching. (Load balancing and failover among the models of a template were added later: section 7.5.)
- Model hosting, fine-tuning, vector storage, agent runtime.

These are not rejected. Section 19 places each of them in a later phase.

### First users

PACOD's four LLM-calling services (`api`, `admin-api`, the improvement-loop worker and the newsletter worker) become API keys in one PACOD workspace. Other applications and customers follow as further organisations and workspaces.

---

## 3. Architecture

```text
  applications                         :8080 data plane             providers
  OpenAI SDK, native clients,          /v1  /gw/v1  /proxy          OpenAI · Anthropic · Gemini
  provider SDKs              ────────►┌──────────────────────┐────► GLM · DeepSeek · Kimi · Ollama
                                      │    pacod-gw-core     │
  UI (SvelteKit)             ────────►│       (Rust)         │──── :9100 /metrics ──► Prometheus ──► Grafana
  scripts, CI                  :8081  │                      │──── OTLP ────────────► OTel collector
                          management  └───┬──────────────┬───┘
                                 API      │              │  events (spool → batches)   core → analytics
                                          │              │  report queries             core → analytics
                                          │              │  job leases and results     analytics → core
                                          ▼              ▼
                                     PostgreSQL   ┌────────────────────┐
                                     config,      │ pacod-gw-analytics │────► ClickHouse
                                     secrets,     │       (Rust)       │      events, audit records, rollups
                                     audit,       │ ingest · rollups   │
                                     counters     │ reports · jobs     │────► price feeds, pricing pages
                                                  └────────────────────┘
```

### 3.1 Components

| Component | Path | Responsibility |
|---|---|---|
| **Core** | `crates/pacod-gw-core` | Data plane (proxy and managed mode), management API, configuration store access, limits, event spool, metrics and traces. |
| **Analytics** | `crates/pacod-gw-analytics` | Event ingest, ClickHouse storage, rollups, report queries, and batch jobs: price tracker, model discovery, retention. Internal only. |
| **Event schema** | `crates/pacod-gw-events` | The usage-event contract between core and analytics. |
| **UI** | `ui/` | Admin console. Talks only to the core's management API. |
| **Stores** | – | PostgreSQL (configuration, encrypted secrets, audit log, limit counters). ClickHouse (usage events, audit-mode records, rollups). |
| **Observability stack** | `deploy/` | OTel collector, Prometheus, Grafana. Configuration, not code. |

The three components the requirements name map to: core, UI, and "analytics plus stores plus observability stack".

### 3.2 Rules that keep the parts separate

- Only the core is reachable from outside. The analytics service and both stores are internal.
- Only the core reads and writes the configuration database. The analytics service gets what it needs through an internal job API on the core.
- The UI calls only the management API. There is no second base URL.
- The request path never waits on PostgreSQL, ClickHouse or the analytics service. If any of them is down, traffic continues on the last snapshot and events accumulate in the spool.

### 3.3 Listeners and roles

| Listener | Port | Serves |
|---|---|---|
| Data plane | 8080 | `/v1`, `/gw/v1`, `/proxy`, `/healthz`, `/readyz` |
| Management API | 8081 | `/api/v1`, including the analytics facade and the internal job API |
| Metrics | 9100 | `/metrics` for Prometheus |
| Analytics (internal) | 8090 | `/ingest`, `/reports` |

The core starts with `--role all` by default. `--role gateway` serves only the data plane and `--role admin` only the management API, so a larger installation can run many gateway instances and keep the management API off the public network. The management listener has its own runtime and concurrency limit, so an expensive admin query cannot slow request handling.

---

## 4. Data plane

### 4.1 Two processing modes

| | Proxy mode | Managed mode |
|---|---|---|
| Entry | `/proxy/{provider}/…` | `/v1/…` (OpenAI-compatible and Anthropic-compatible) and `/gw/v1/…` (native) |
| The client speaks | the provider's own API, usually with the provider's SDK | OpenAI chat completions, Anthropic Messages or the native interface |
| Request and response bodies | relayed byte for byte; with a template, only top-level parameter fields of the request are written (section 7.4) | parsed and validated; governed fields are rebuilt, and conversation content is kept as sent when the dialect matches (section 4.3) |
| Model is named by | the provider's model id, or `@template` | a repository model id or `@template` |
| Templates | yes: model, parameters and ceilings | yes |
| Capability filter on the caller's own parameters | no | yes |
| Dialect translation | no | yes |
| Retired model follows its successor | only through a template, to a successor at the same provider | yes |
| Retries | only when nothing has been sent yet | before the first response byte; never after a timeout |
| Usage and cost | read from the response when the provider reports it | always |
| Limits | checked at admission, settled afterwards | same |
| Audit mode: full record of requests and responses (section 14.6) | yes | yes |
| Gateway delay measured and stored per call (section 14.2) | yes | yes |
| Use it for | maximum speed, provider SDKs, provider features the gateway does not model | everything else |

A key carries `allowed_modes`. A request cannot choose its mode, because that would let a caller step around templates and parameter rules. A key can also require a template in proxy mode (section 7.4).

### 4.2 Proxy mode

The promise has two levels:

- **Without a template,** request and response bodies are never altered.
- **With a template,** the conversation content (messages, system prompt, tool definitions, attachments) and the whole response are still never altered. Only top-level parameter fields of the request are written, such as the model and the token limit (section 7.4).

Headers are a different matter in both cases: the gateway must replace the credential, and it adds its own response headers.

#### Pipeline

1. Authenticate the gateway key. It is accepted in `Authorization: Bearer` and also in the provider's own header (`x-api-key` for Anthropic, `x-goog-api-key` for Gemini), so provider SDKs work with only the base URL and key changed.
2. Resolve `{provider}` and check that the key's workspace may use it and that the path is on the provider's allow-list.
3. Buffer the request body once, up to a configured size, and scan it without building a document tree to locate its top-level fields. The gateway reads `model`, `stream` and `stream_options`. A body that repeats a top-level key the gateway reads or writes is rejected, so the gateway and the provider cannot disagree about what was asked for.
4. If the request names a template, resolve it and write its model and parameters into the top-level fields (section 7.4). Otherwise the body is forwarded as the same bytes.
5. Check the model against the workspace's grants, then rate limits, then that no applicable quota is already exhausted.
6. Replace the auth header with the provider credential, set `Host`, drop hop-by-hop and `x-gw-*` headers, and forward.
7. Relay response chunks as they arrive. Each chunk is handed to the client first; a reference-counted copy then goes to a per-dialect usage extractor.
8. Settle: update counters, compute cost, write the usage event.

#### Usage when nothing may be injected

| Dialect | Usage in the response |
|---|---|
| OpenAI chat, non-streaming | Always. |
| OpenAI chat, streaming | Only if the client sent `stream_options.include_usage`. |
| Anthropic Messages | Always: input side in `message_start`, output side in `message_delta`. |
| Gemini native | In `usageMetadata` (to verify). |
| Ollama native | In the final chunk (to verify). |
| GLM, DeepSeek, Kimi (OpenAI-compatible) | As OpenAI (to verify per provider). |

For the case with no usage, each key has a `usage_policy`:

| Policy | Behaviour |
|---|---|
| `require` | Reject before forwarding with `400 usage_required`. Default whenever a token or spend limit applies to the key. |
| `estimate` | Forward. Record an estimate from byte counts, marked `usage_source: estimated`. |
| `ignore` | Forward. Count the request; tokens and cost stay empty. |

Every event records `usage_source` as `provider`, `estimated` or `missing`, so reports can show how much of the spend is exact.

A template can close this gap itself: with `request_stream_usage` it adds `stream_options.include_usage` to OpenAI-dialect streams, so usage is always reported (section 7.4).

**What proxy mode cannot do,** with or without a template: translate between dialects, filter the caller's own parameters, validate or shape the response, fall back to another provider, put cost in the response body, or reserve tokens exactly in advance. Without a template it also cannot use repository model ids or follow a retired model to its successor.

Any provider path on the allow-list is relayed, including endpoints the gateway does not model (embeddings, files, batches, token counting). Accounting is exact on paths that have a usage extractor and request-count only on the rest.

An optional **blind relay** (`inspect: none` on a key) skips step 3 and streams the body without buffering. It is only allowed for keys with no model restriction and `usage_policy: ignore`, and templates cannot be used with it.

### 4.3 Managed mode

1. Authenticate the key; this yields workspace and organisation.
2. Parse the request into the canonical model. The native interface maps one to one; the OpenAI and Anthropic dialects each have a mapping.
3. Resolve the target: `@template` → template version → model, or a model id checked against the grants. Life-cycle rules apply here (section 6.3).
4. Merge parameters from the model defaults, the template and the caller (section 7.2).
5. Admission: rate limits, quotas.
6. Capability filter: parameters the model does not accept are dropped or the request is rejected, depending on policy. What was dropped is reported.
7. The adapter builds the provider request and attaches the credential (workspace, then organisation, then global).
8. Send, with the reliability rules of section 4.4.
9. Decode the response or stream into canonical events and encode them in the caller's dialect.
10. Settle: usage, cost, counters, event, metrics.

In managed mode the gateway always asks the provider for streamed usage. It passes the usage chunk on to an OpenAI-dialect caller only if that caller asked for it; the native interface always carries it.

**Same dialect in and out.** When the caller's dialect is the provider's, the conversation content (the messages, the system prompt and the tool definitions) is forwarded byte for byte, and only the fields the gateway governs are rewritten. This is a requirement, not an optimisation: provider prompt caches match on exact bytes, and Anthropic's thinking blocks are valid only in an unedited history (section 8.3). It also means provider features the gateway does not model keep working in managed mode. The only exceptions are changes the capability filter or a provider's quirk profile demands, such as removing cache markers a provider rejects or normalising history for Gemini.

### 4.4 Reliability rules

| Rule | Detail |
|---|---|
| A timeout is never retried | The provider may have finished and billed the call. This holds at every layer. The response is marked non-retryable (`retryable: false`, and `x-should-retry: false` for SDKs that honour it, to verify). |
| Retries happen before the first byte | Connection errors, 429 and 5xx are retried with backoff and jitter, up to the template's `retry.max_attempts` (default 2). Once a byte of the response has been sent to the client there is no retry. |
| A broken stream is reported | The client gets an error event, the event is recorded with `error_type: stream`, and tokens received so far are accounted. |
| One outcome per logical call | The event carries `attempts`; retries are counted separately in a metric. |
| Timeouts are sized per template | `timeout_seconds` up to 600. A stream-idle timeout ends silent connections. |
| No implicit load balancing | Model ids are unique. Two entries can never become a hidden rotation. |
| No fallback unless configured | A template with several models moves a call on to the next one its routing scheme names, on 429, 5xx or a lost connection, before the first byte (section 7.5). A template with one model fails as before. Either way the outage is visible. |
| Client disconnect cancels upstream | The upstream request is aborted and the event records `cancelled`. |
| Back-pressure | A per-instance cap on in-flight streams; above it the gateway answers `503 gateway_overloaded` with `Retry-After`. |

Applications should run one retry layer, not two. Either the application disables its SDK retries, or the template sets `retry.max_attempts: 1`.

### 4.5 Performance design and targets

Design choices that keep the core light:

- Tokio with hyper for both the server and a pooled client (HTTP/2 and keep-alive HTTP/1.1, rustls). Response chunks are relayed as reference-counted buffers without copying.
- Reading configuration is one atomic pointer load. Key lookup is a hash-map probe on an HMAC.
- Limits are atomic counters in memory. Events go into a bounded channel.
- Proxy mode builds no JSON tree. Managed mode parses once.
- No per-request allocation of provider clients; pools are per provider base URL.

Targets, to be validated by a load test before they are treated as commitments:

| Attribute | Target |
|---|---|
| Added latency, proxy mode | p50 ≤ 0.5 ms, p99 ≤ 2 ms |
| Added latency, managed mode, same dialect | p50 ≤ 1 ms, p99 ≤ 5 ms |
| Added latency, managed mode with translation | p99 ≤ 8 ms |
| Concurrent streams per instance (2 vCPU, 1 GB) | 10 000 proxy, 5 000 managed |
| Idle memory per instance | ≤ 50 MB |
| Container image | ≤ 40 MB |
| Configuration change visible | immediately on the instance that took the write; ≤ 2 s elsewhere |
| Key revocation effective | ≤ 2 s on every instance |
| Events lost on a process crash | at most the last 100 ms |

These latencies are the gateway delay defined in section 14.2. It is measured and stored for every call, so the targets can be checked against live traffic and not only in a load test.

The assessment's warning stands: provider calls take seconds, so once these budgets are met further tuning is not a goal. Correct accounting and predictable behaviour matter more than microseconds.

---

## 5. Organisations and workspaces

### 5.1 Entities

- **Organisation.** A customer or a large internal unit. It groups workspaces and holds administrators, a set of granted models, optional own provider credentials and an overall cap.
- **Workspace.** The unit applications work in: one application, project, environment or team. It owns API keys, templates, limits, settings and its usage.
- **API key.** Belongs to one workspace. One key per calling service is the norm.
- **End user, session, trace, tags.** Attributes on a request, not stored entities. They attribute usage inside a workspace.

A built-in `_system` organisation and workspace carry the gateway's own LLM calls (price extraction, probes), so their cost is visible like any other.

### 5.2 What lives where

| Object | Global | Organisation | Workspace | Key |
|---|---|---|---|---|
| Providers, models, capability profiles, prices | defined here | – | – | – |
| Provider credentials | default | optional override | optional override | – |
| Model grants | – | which global models | subset of the organisation's | optional subset |
| Templates | – | – | defined here | optional allow-list |
| Limits (tokens, requests, spend, rate) | – | cap | yes | yes, also per end user |
| Settings (unsupported-parameter policy, audit mode, retention, tag keys, allowed provider tools) | default | default, can be locked | yes | – |
| Usage, errors, audit-mode records | – | roll-up | owner | attributed |

### 5.3 Inheritance

Two rules cover everything that restricts:

- **Allow-lists intersect.** A workspace can use a model only if the organisation has it and the workspace has it; a key can narrow further.
- **Numeric caps take the minimum.** Every applicable limit must pass.

Settings follow a third, simpler rule: the nearest defined value wins (workspace, then organisation, then global), unless the organisation locked it.

### 5.4 Roles

| Role | Scope | Can |
|---|---|---|
| Platform admin | everything | manage providers, credentials, models, prices, organisations, global settings |
| Organisation admin | one organisation | manage its workspaces, members, grants within what it was given, own credentials, caps |
| Workspace admin | one workspace | manage templates, keys, limits, settings, audit mode; read usage and audit-mode records |
| Auditor | organisation or workspace | read configuration, usage, audit-mode records and the audit log; change nothing |
| Viewer | organisation or workspace | read configuration and usage, no secrets, no audit-mode records |

The first release has local accounts and scoped admin tokens for automation. OIDC login follows in phase 2. With an SMTP server configured, local accounts confirm their email address before the first login and reset a forgotten password through an emailed one-time link; "keep me logged in" extends a console session from hours to days (runbook section 13).

### 5.5 API keys

- Format `pgw_<key id>_<secret>`: recognisable in logs and by secret scanners, with enough random bits that guessing is not a threat.
- Stored only as HMAC-SHA256 with a server-side pepper and looked up directly in the snapshot. No slow password hash on the request path.
- Attributes: name, workspace, `allowed_modes`, optional model and template allow-list, `require_template`, rate class, `usage_policy`, expiry, status.
- The secret is shown once at creation. Rotation creates a new secret and keeps the old one valid for a grace period. Revocation takes effect within two seconds.

### 5.6 Attribution inside a workspace

| Attribute | Purpose |
|---|---|
| `end_user` | The person or account the call is made for. Drives end-user limits and per-user reports. |
| `session_id` | A conversation. |
| `trace_id` | One unit of work, such as a chat turn. All calls with the same trace are shown and totalled together. |
| `tags` | Free key-value pairs such as `purpose`, `site`, `profile`. A workspace declares its tag keys (up to eight); undeclared keys are dropped with a warning. |
| `agent_id` | The agent that makes the call. |
| `user_id` | The platform user the agent acts for. A separate dimension from `end_user`: it is reported and filtered on, but does not drive end-user limits or the end-user reports. |
| `task` | What the call is for, as a short label (up to 64 characters). |
| `complexity` | How demanding the caller says the call is: `low`, `medium` or `high`. Any other value is refused with `400`. |

The last four are columns of their own, not tags: they need no declaration in the workspace, do not count against its eight tag keys, and are the same in every workspace. All four are optional. They are reported, filtered and grouped on like `end_user`, and `complexity`, a closed set, is also a metric label (section 14.7).

For PACOD: one organisation, one workspace per environment, `end_user` = user id, `trace_id` = turn id, and tags for purpose, site and profile.

---

## 6. Model repository

The repository is global. It answers three questions: which providers exist and how to reach them, which models can be called and what they accept, and what state each model is in.

### 6.1 Entities

| Entity | Main fields |
|---|---|
| **Provider** | slug, adapter family (`openai`, `anthropic`, `gemini`, `ollama`), base URL, auth scheme, quirk profile, default timeout, status, health |
| **Credential** | provider, scope (global, organisation, workspace), label, encrypted secret, fingerprint, version, status |
| **Model** | id (unique, free-form, for example `anthropic/claude-sonnet-5-5`), provider, upstream id, kind (chat, speech, later embedding and others), modalities, context window, maximum output, capability profile, state, successor, deprecation and sunset dates |
| **Capability profile** | which parameters the model accepts and how (section 6.2) |

A model row is one provider endpoint for one upstream model. The same upstream model reached through two providers is two rows with two ids. PACOD's existing ids (`anthropic/…`, `openai/…`, `glm/…`, bare Gemini names) can be kept unchanged.

### 6.2 Capability profiles

A capability profile is data, editable through the API, not code. It replaces PACOD's hand-maintained capability table and LiteLLM's silent `drop_params`.

| Field | Values |
|---|---|
| `temperature`, `top_p`, penalties | accepted, rejected, or accepted only at the default value |
| `reasoning.style` | none, budget (token budget), adaptive (effort only), always on |
| `reasoning.efforts` | the effort levels the model accepts |
| `reasoning.min_budget` | minimum token budget for budget-style models |
| `prompt_cache` | explicit markers, automatic, none |
| `vision`, `tools`, `server_tools`, `structured_output` | supported or not |
| `tool_choice_forced`, `assistant_prefill` | supported or not |
| `max_output_tokens` | hard ceiling |

Examples from Anthropic's current line show why this must be data: recent Opus models reject sampling parameters outright; Claude Sonnet 5.5 accepts them only at their default value; token budgets for thinking are rejected on recent models and required on Haiku 4.5; thinking cannot be switched off on the newest ones. New models arrive every few weeks, and a profile edit takes effect without a release.

Profiles are seeded from built-in family presets. The management API returns the profile with each model, so the UI shows only the knobs that will be honoured.

### 6.3 Life cycle

```text
            activate               deprecate               retire
  draft ───────────────► active ───────────────► deprecated ───────────────► retired
                           │  ▲                     │  ▲
                   disable │  │ enable      disable │  │ enable
                           ▼  │                     ▼  │
                               disabled (reversible)
```

| State | Callable | Listed to workspaces | New templates may use it | Behaviour |
|---|---|---|---|---|
| `draft` | no | no | no | Being set up. Can be edited freely and deleted. |
| `active` | yes | yes | yes | Normal. |
| `deprecated` | yes | yes, marked | no | Responses carry `x-gw-model-deprecated` with the sunset date and successor. |
| `retired` | not itself | no | no | Requests resolve to the successor if the grant allows it, otherwise `410 model_retired`. |
| `disabled` | no | yes, marked | no | Emergency off switch. Requests fail with `503 model_disabled`. Returns to the previous state. |

#### Gates and rules

- **Activation** needs a capability profile, a price or an explicit "free" marking, a usable credential and a successful probe call.
- **Deprecation** can name a successor and a sunset date. A scheduled job retires the model on that date if asked to.
- **Retirement does not rewrite anything.** Templates and settings keep naming the retired model. At request time the core follows the successor chain, re-validates the parameters against the successor's capability profile, and records both `model_requested` and `model_served`. PACOD's boot-time rewrite of settings is replaced by this, and it is reversible.
- **Only drafts can be deleted.** Everything else is retired, so usage history keeps resolving.
- **Every transition has a dry run** that lists the organisations, workspaces, templates and keys affected, with their request counts for the last 30 days.
- **Every transition is audited.**

### 6.4 Discovery and health

- **Discovery.** A job lists each provider's models (OpenAI-compatible `/v1/models`, Anthropic `GET /v1/models` which also returns context window, output limit and capabilities, Ollama's local model list). New upstream ids become proposals for drafts; ids that disappeared are flagged. Nothing is activated automatically.
- **Health.** Passive: error rate and latency per model from live traffic. Active: a probe on demand and at activation. Health is displayed and alerted on. Routing among the models of a template keeps its own circuit breaker per instance (section 7.5); the health page still counts every failure, including those another model absorbed.

### 6.5 Provisioning to workspaces

A model becomes usable in a workspace through two grants: the platform admin grants it to the organisation, and the organisation admin grants it to the workspace. A grant can carry `follow_successor` (default on), a lower output ceiling, and later a contract price.

`GET /v1/models` on the data plane returns exactly what the calling key can use: granted, callable models and the workspace's templates.

---

## 7. Workspace templates

A template is a named, preconfigured LLM for one purpose in one workspace: which model, with which parameters, under which limits. Applications then send a name instead of a parameter set. Templates work in both modes; section 7.4 describes what they do in proxy mode.

### 7.1 Definition

| Field | Meaning |
|---|---|
| `name` | Slug, unique in the workspace. Addressed as `@name`. |
| `model` | A model granted to the workspace. |
| `models` | Instead of `model`: a pool of up to 16 models, each with a tier, a weight and a priority (section 7.5). |
| `routing_scheme` | The organisation's routing scheme that chooses among `models`. Unset: the workspace's, else the organisation's. |
| `max_output_tokens` | Default and ceiling. |
| `temperature`, `top_p`, `frequency_penalty`, `presence_penalty` | Sampling parameters. |
| `reasoning` | `effort`, optional `budget_tokens`, and whether a summary of the reasoning is returned. |
| `timeout_seconds` | Up to 600. |
| `retry.max_attempts` | Attempts before the first byte. |
| `prompt_cache` | `off`, `passthrough` (honour the caller's cache markers), or `auto` (the provider places the marker; available for Anthropic models). |
| `unsupported_params` | `drop` or `reject`; overrides the workspace setting. |
| `audit` | Audit mode for calls using this template: `inherit`, `on` or `off` (section 14.6). |
| `tags` | Tags added to every call, for example `purpose`. |
| `proxy` | How the template acts in proxy mode: `apply` (default) or `check`. |
| `request_stream_usage` | Proxy mode only: ask the provider to report usage on OpenAI-dialect streams. Managed mode always does. |

Each parameter has three possible states: **unset** (use the model's default), **omit** (never send it), or a **value**. Each value is either a **default** the caller may override or **locked**.

### 7.2 How parameters combine

1. **Values.** Model default, then template default, then the caller's value, then a template's locked value. Later wins.
2. **Ceilings.** For output tokens, reasoning budget and timeout the result is the minimum of the template's ceiling, the grant's ceiling and the model's maximum.
3. **Capability filter.** Runs last and is final. A parameter the model does not accept is dropped or the request is rejected, per `unsupported_params`. Dropped names are returned in `x-gw-dropped-params` and in the response's `gateway` block.

Example: template `@synthesizer` sets `temperature: 0.3` as a default on `anthropic/claude-sonnet-5-5`, and the caller sends nothing. The merge yields 0.3; the capability profile says this model takes temperature only at its default, so the parameter is dropped, the call succeeds and the response reports `dropped_params: ["temperature"]`.

### 7.3 Versions and life cycle

- Every save creates a version. Calls use the current version; events record template and version. Rolling back means making an earlier version current.
- A template cannot be created on a draft, deprecated, retired or ungranted model (`422`). An existing template on a deprecated model is flagged in the UI. On a retired model it follows the successor or fails, as the grant says.
- Deleting a template that a key's allow-list names returns `409` with the keys. The UI shows when each template was last used.
- Templates can be cloned to another workspace and exported or imported as JSON. Organisation-level shared templates are a phase 2 item.

For PACOD, each LLM role (planner, synthesizer, loop judge and so on) becomes a template. PACOD keeps deciding which role uses which template per profile; it stops sending the parameter set and sends `@name`.

### 7.4 Templates in proxy mode

Proxy mode does not parse the conversation, so a template cannot do everything there that it does in managed mode. It does what a template is for: it chooses the model, sets the parameters and enforces the ceilings.

#### Naming the template

Either of these:

- `model: "@name"` in the request. For Gemini's native API the name goes in the model segment of the URL.
- The `x-gw-template` header.

The template's model must belong to the provider in the URL. Otherwise the request is rejected with `400 template_provider_mismatch`.

#### What the template does

| Template field | Effect in proxy mode |
|---|---|
| `model` | The provider's model id is written into the request. A retired model is replaced by its successor, if that is at the same provider. |
| `max_output_tokens` | Written when the caller sent none; lowered when the caller's value is above the ceiling. |
| Sampling parameters, `reasoning` | A default is written when the caller sent none. A locked value overwrites the caller's. "Omit" removes the field. All are written in the provider's own field names. |
| `prompt_cache` | `auto` adds Anthropic's top-level cache marker. `off` cannot be enforced, because the caller's markers sit inside the content. |
| `request_stream_usage` | Adds `stream_options.include_usage` to OpenAI-dialect streams. The client then receives one extra final chunk carrying usage. |
| `timeout_seconds`, `retry.max_attempts` | Applied by the gateway. A retry is still possible only while nothing has been sent. |
| `audit`, `tags` | Applied. |
| `unsupported_params` | Does not apply. The template's own values are checked against the capability profile when the template is saved, so only accepted parameters are ever written. The caller's own parameters are not filtered. |

Which request fields a template may write depends on the provider family:

| Family | Fields |
|---|---|
| OpenAI-compatible | `model`, the output-token limit, `temperature`, `top_p`, the two penalties, `reasoning_effort`, `stream_options` |
| Anthropic | `model`, `max_tokens`, `temperature`, `top_p`, `thinking`, `output_config`, the top-level `cache_control` |
| Gemini native | the model in the URL, `generationConfig` (to verify) |
| Ollama native | `model`, `options` (to verify) |

#### How it stays fast and leaves the content alone

- The scan that proxy mode already performs gives the byte range of every top-level field. The new body is the original bytes with parameter fields replaced, added or left out.
- The content fields (`messages`, `system`, `tools`, and their equivalents in other dialects) are copied as bytes. They are never parsed and never re-serialised.
- Small parameter objects such as `output_config` or `generationConfig` are parsed and merged. They are a few bytes long.
- The response is never changed.
- Because the content is byte-identical, provider prompt caches keep hitting and Anthropic's thinking blocks stay valid (section 8.3).
- The cost is one splice over a body that is already buffered. The latency targets of section 4.5 include it.
- This applies to JSON request bodies. On other bodies, such as multipart uploads, a template contributes only its gateway-side settings.

#### Apply or check

| `proxy` setting | Behaviour |
|---|---|
| `apply` (default) | Writes the fields as described above. |
| `check` | Writes nothing. The request must already use the template's model, by the provider's id, and respect the locked values and the ceilings. Otherwise it is rejected with `400 template_violation`. The template is named in the header. |

`check` keeps the strict promise of an unaltered body and still ties a key to an approved configuration.

#### Requiring a template

A key with `require_template` accepts proxy requests only when they name a template the key may use. That gives proxy mode the same control over models and ceilings that managed mode has.

#### What stays different from managed mode

Even with a template, proxy mode does not validate the request, filter the caller's own parameters, translate between dialects or shape the response. Events record the template and its version in both modes, so reports by template cover both.

### 7.5 Pools and routing schemes

A template may hold several models instead of one: its **pool**. A **routing scheme** decides, per call, which of them serves first and which take over when it fails, from what the caller says about the call. The gateway still never balances or falls back on its own: a pool and its scheme are explicit configuration, and every decision is reported.

#### The pool

| Field of an entry | Meaning |
|---|---|
| `model` | A model granted to the workspace. Up to 16 entries, each model once. |
| `tier` | A label schemes choose by, for example `small` or `large`. Optional. |
| `weight` | Share of calls for the weighted algorithms. Default 1; `0` makes the model a backup only. |
| `priority` | Main and backups: lower comes first. Default 0. |

A template with `model` behaves as a pool of one; its documents are unchanged.

#### The scheme

A scheme belongs to an organisation and is assigned to the organisation, a workspace (their `settings.routing_scheme`) or a template (`routing_scheme`). The organisation's assignment and its lock are its administrators' to set, like the organisation's other settings (`PUT /orgs/{org}/routing-assignment`); they also define the schemes and assign them to workspaces and templates. The most specific assignment of an existing, active scheme applies; an organisation that lists `routing_scheme` in `settings.locked` decides for all its workspaces and templates. Without any, the **built-in routing** applies: the whole pool by priority, up to three models.

Rules are tried in order; the first whose conditions all hold decides, otherwise the scheme's `default` applies.

| Condition | Matches |
|---|---|
| `complexity` | Any of `low`, `medium`, `high`, `unset` (none stated). |
| `agent_id`, `task`, `user_id`, `template` | `in` (exact values), `prefix`, or `present: true/false`. |
| `tags` | The same, per tag key. |
| `input_tokens` | `at_least` and `below`, estimated from the size of the body (about four bytes a token). |
| `cel` | A CEL expression over the same values, for what the typed conditions cannot say. It is checked last; one that fails to evaluate does not match. Bounded when compiled: at most 512 characters, a fixed list of variables and functions, no loops. |

| Action | Meaning |
|---|---|
| `tiers` | Tiers of the pool in order of preference; empty for the whole pool. A rule naming tiers a pool lacks is skipped for that template. |
| `algorithm` | How the first model is picked within the first tier that has a usable model: `priority` (main and backups), `weighted`, `round_robin`, `weighted_round_robin` (smooth, evenly interleaved), `least_in_flight` (of two weighted picks, the less busy), `lowest_latency` (measured time to first byte, with 5 % exploration), `cheapest` (input plus output price). |
| `affinity` | `session`: calls with the same `session_id` stay on one model while it is healthy (weighted rendezvous hashing), which keeps the provider's prompt cache warm. |
| `overflow` | `rest_of_pool`: after the chosen tiers, the rest of the pool by priority. |
| `failover` | Overrides the scheme's failover policy for this rule. |

The models after the first follow in order: the rest of its tier by priority, the later tiers, then the overflow. A canary is a weighted rule with a small weight. Hedged requests and shadow traffic are not planned: they double the cost.

#### Failover

A call moves on to the next model only before any byte of the answer has reached the caller.

| `failover.on` | Default |
|---|---|
| `rate_limited` (429) | on; a `Retry-After` also sets the model aside for that long |
| `server_error` (5xx, Anthropic's 529) | on |
| `connect_error` (no connection, no credential for the provider) | on |
| `limit_refused` (a gateway limit that counts only this model) | on |
| `timeout` | off: the provider may be generating, and billing, the answer already |

`max_targets` (1 to 8, default 3) bounds the models one call tries. With a pool each model gets `attempts_per_target` (default 1: the next model is the better retry); a pool of one keeps the template's `retry.max_attempts`. The template's timeout applies per attempt as before; `per_target_timeout_ms` caps it and `deadline_ms` stops trying further models. `x-gw-retries` caps the attempts of the whole call. A 4xx other than 429 is the caller's to see and is not retried elsewhere.

Each model a call moves to is resolved as if it had been named: grants, the key's allow-list, the life cycle and the provider's state apply, and the request is shaped or translated for that model's dialect. Limits that count the call on both models stay counted once; those of the model it left are released, and its tokens and spend are settled against the model that served. A model whose context window the input clearly exceeds is passed over while another one fits.

Proxy mode routes only among the pool's models at the provider in the URL and does not fail over: it relays the provider's answer. A checking template accepts any of its models there.

#### Health

Each instance keeps, per model and credential, a circuit breaker, the calls in flight and a moving average of the time to the first byte. After `GlobalSettings.breaker.failures` failures in a row (default 5) a model goes last in every order for `cooldown_seconds` (30), doubling up to `max_cooldown_seconds` (300); then one probe call decides. A model is never skipped because every model looks unhealthy: the order only changes. This state is per instance, so round robin is approximate across instances, and only managed calls feed it.

#### Speed

A scheme is compiled into the configuration snapshot and bound to each template's pool, so a call evaluates precompiled conditions over precomputed candidate lists, without lookups, locks or allocations. Measured medians: about 60 ns with ten rules, 160 ns with 64, 650 ns with a CEL rule; a template with one model adds about 60 ns to naming the model directly.

#### What is reported

`x-gw-route` and `x-gw-failovers` on the response; on the usage event `routing_scheme`, `routing_scheme_version`, `routing_source`, `routing_rule`, `failovers` and `models_tried`, which reports can filter and group by; in the audit record the candidates, the models passed over and every model tried with its status. The metrics `gw_route_failovers_total`, `gw_breaker_open` and `gw_route_cel_errors_total`. `POST …/templates/{id}/route` explains how a call would be routed, `GET /monitoring/routing` shows the breakers.

---

## 8. Providers and adapters

### 8.1 The seven providers

Base URLs and provider-specific behaviour below are starting values, to verify with fixtures.

| Provider | Adapter in managed mode | Default base URL | Auth | Notes |
|---|---|---|---|---|
| OpenAI | OpenAI-compatible | `https://api.openai.com/v1` | Bearer | Chat completions and speech. Reasoning models restrict sampling parameters and take `reasoning_effort`. Prompt caching is automatic; cached tokens appear in `prompt_tokens_details`. |
| Anthropic | Anthropic, in full (section 8.3) | `https://api.anthropic.com` | `x-api-key` plus `anthropic-version: 2023-06-01` | The whole Messages API: tools, thinking, explicit and automatic prompt caching, server tools, citations, documents, structured output, token counting, batches, files. Input tokens exclude cached tokens. |
| Google Gemini | OpenAI-compatible (Gemini's compatible endpoint) | `https://generativelanguage.googleapis.com/v1beta/openai` (to verify) | Bearer | Native Gemini API through proxy mode from the start; native adapter in phase 2. Needs message-history normalisation. Reasoning and cached-token reporting on the compatible endpoint to verify. |
| GLM (Zhipu / z.ai) | OpenAI-compatible | `https://api.z.ai/api/paas/v4` | Bearer | Reasoning text arrives as `reasoning_content`. Some reasoning models reject a non-default temperature. |
| DeepSeek | OpenAI-compatible | `https://api.deepseek.com` (to verify) | Bearer | `reasoning_content`; cache hit and miss token fields with their own names (to verify). |
| Kimi (Moonshot) | OpenAI-compatible | `https://api.moonshot.ai/v1` (to verify) | Bearer | Reasoning and cache fields to verify. |
| Ollama | OpenAI-compatible (`/v1`) | `http://<host>:11434/v1` | none by default | Native `/api` through proxy mode. Local, so the price is "free" or an admin-set rate. Must be reachable only from the gateway. |

### 8.2 Adapter families

**OpenAI-compatible.** One adapter serves six providers. They differ by a **quirk profile**, which is data: the auth header, the name of the reasoning field in stream deltas, where cached and reasoning tokens are reported, which extra parameters exist, whether history must be normalised. Adding a seventh OpenAI-compatible provider is configuration.

**Anthropic.** A complete adapter and a complete client dialect. Section 8.3 defines what complete means and how it is achieved.

Translation between the two families runs through the canonical model in both directions, so each interface can reach each provider. What one side cannot express is answered with `400 unsupported_feature` rather than silently ignored: for Anthropic models that is `n > 1`, log-probabilities and JSON mode without a schema.

### 8.3 Anthropic in full

"In full" means three things:

1. **Nothing is lost on the way to Anthropic.** Whatever the Anthropic Messages API accepts can be sent through the gateway's Anthropic-compatible interface (section 9.4) to an Anthropic model, and comes back as Anthropic returned it. The gateway governs a short list of fields and forwards everything else untouched, so features Anthropic adds later work without a gateway release.
2. **Anthropic models are complete from the other interfaces.** From the OpenAI-compatible and native interfaces, everything those interfaces can express is translated in both directions: tools, structured output, reasoning across turns, caching, documents, citations, server tools.
3. **The Anthropic interface is a full client dialect.** Clients built on Anthropic's SDK get templates, limits, accounting and audit mode like any other client, and can also call models of the other six providers for the common feature set.

Proxy mode remains available for Anthropic as for every provider. The difference is that managed mode no longer costs any Anthropic feature.

#### Three rules that make it work

| Rule | Why |
|---|---|
| **History is never rewritten.** On the Anthropic → Anthropic path the `system`, `tools` and `messages` values are forwarded byte for byte. The gateway changes only fields outside them. | Prompt caching matches the exact bytes of the prefix, so re-serialising would turn cache reads into cache writes. Thinking blocks must be returned unchanged, and the newest models can reject a conversation whose earlier turns were edited. Fields outside those three values (`max_tokens`, effort, `tool_choice`, `metadata`) can change without either effect. |
| **Translation is deterministic.** On translating paths the same input always produces the same bytes. | The same two reasons, for clients on the other interfaces. |
| **Unknown is forwarded, not dropped.** Request fields, content-block types, stream events and response fields the gateway does not recognise pass through. On the other interfaces they appear as opaque provider blocks that the client sends back as received. | New API features, and blocks that must be echoed intact, such as compaction and fallback blocks. |

#### What the gateway governs on an Anthropic request

`model`, `max_tokens`, `temperature`, `top_p`, `top_k`, `stop_sequences`, `thinking`, `output_config.effort`, `tool_choice`, `stream`, the tool types in `tools` (checked against the workspace's allowed provider tools), `metadata.user_id` (read as the end user when none is given) and the `gateway` object (removed before forwarding). Templates and the capability filter act on these and on nothing else.

#### Feature coverage

| Feature | Anthropic interface → Anthropic model | OpenAI-compatible or native → Anthropic model | Anthropic interface → other provider |
|---|---|---|---|
| Text, images, streaming | unchanged | translated | translated |
| Documents (PDF, text) and file references | unchanged | native `document` block; OpenAI file parts (to verify) | where the model supports it |
| Client tools: definitions, parallel calls, results, `tool_choice`, strict schemas | unchanged | translated both ways | translated |
| Anthropic-defined client tools (bash, text editor, computer use, memory) | unchanged | provider tools, passed verbatim | not available |
| Server tools (web search, web fetch, code execution, tool search) and the MCP connector | unchanged, if the workspace allows the tool type | provider tools; results come back as provider blocks | not available |
| Thinking and effort | unchanged, capability-checked | unified `reasoning`; thinking blocks are returned with their signature and accepted back | mapped to the model's own reasoning control |
| Prompt caching: block markers, 1-hour lifetime, automatic placement | unchanged | the caller's markers are kept; `auto` uses Anthropic's automatic placement | markers removed |
| Citations | unchanged | `citations` on text blocks | not available |
| Structured output | unchanged | a JSON schema in `response_format` ↔ `output_config.format` | translated where supported |
| Stop reasons, including `refusal` with its details and `pause_turn` | unchanged | mapped (section 8.4), original kept | mapped |
| Beta features (`anthropic-beta`) and fields the gateway does not model | forwarded | through `provider_options.anthropic` | dropped or rejected per policy |
| Token counting | `POST /v1/messages/count_tokens` | – | `unsupported_feature` |
| Message Batches and Files | governed relay, below | – | – |

Tool types are passed by name, so a new version of a server tool needs no gateway change.

#### Translation details that are easy to get wrong

- **`max_tokens` is required by Anthropic.** When an OpenAI-dialect caller gives none, the template's value or the model default is used.
- **Thinking differs by model.** Adaptive models take `thinking: {type: "adaptive"}` with `output_config.effort`; budget models take `thinking: {type: "enabled", budget_tokens}`. Recent models return no reasoning text unless asked, so the gateway requests a summary when the caller wants reasoning.
- **Thinking blocks cross turns.** On the native interface they are `reasoning` blocks with a signature. On the OpenAI dialect they travel in `provider_specific_fields` on the assistant message and must be sent back with it.
- **Tool results are grouped.** Anthropic expects all results of one assistant turn in a single user message; consecutive OpenAI `tool` messages are merged into one.
- **Forced tool choice and assistant prefill** are rejected by the newest models. The capability profile knows this, and the request is rejected or the setting dropped per policy. It is never sent upstream to fail there.
- **A paused turn** (`pause_turn`) is returned as it is, and the client continues by sending the conversation again. Resuming inside the gateway for clients that cannot do this is a phase 2 option.
- **A refusal** is a successful response with `stop_reason: refusal`. It is recorded with finish reason `content_filter`, not as an error.
- **Server-side fallback.** When Anthropic serves a request with another model, the response names the model that answered. The event records it as `model_served` and prices it accordingly (per-model usage breakdown to verify).

#### Accounting

- Usage is read from `message_start` and `message_delta` on streams: uncached input, cache reads, cache writes split by lifetime, output, and server-tool calls (to verify).
- Prices hold explicit rates for cache reads and for 5-minute and 1-hour cache writes, because the ratio to the input rate differs by model.
- Batch processing and fast mode have their own rates (section 13.1). The variant is taken from what the response reports.
- Token-counting calls are free and recorded as requests.

#### Batches and Files

Provider credentials are normally shared between workspaces, and Anthropic separates files and batches by credential only. The gateway therefore adds ownership:

- **Files.** Upload, list, read and delete are relayed. The gateway records which workspace created each file id, filters lists, and rejects a message that references another workspace's file.
- **Batches.** Each item passes through the same resolve, merge and filter steps as a single message. The batch belongs to the workspace that created it. Limits are checked at creation. When the batch ends, a job reads the results, writes one event per item at the batch rate and settles the counters.

These two are the first Anthropic items to defer if time is short. Proxy mode with a workspace's own Anthropic credential covers them in the meantime.

#### Headers

- `anthropic-version` is forwarded as sent, and set to `2023-06-01` for callers on other interfaces.
- `anthropic-beta` values are forwarded. A platform setting can restrict them.
- Anthropic's request id is returned as `x-gw-upstream-request-id`.
- Anthropic's rate-limit headers describe the provider key. They are passed on only when the credential belongs to the caller's own organisation or workspace.

#### Not part of this

- **Claude on Amazon Bedrock, Google Vertex AI and Microsoft Foundry.** The same Messages shape with different authentication, model ids and feature availability. They follow in phase 2 as variants of the Anthropic family.
- **Managed Agents, the Admin API and the Skills API.** These are separate surfaces. Proxy mode can relay them, without usage accounting.

### 8.4 Normalisation

#### Finish reason

| Canonical | OpenAI dialect | Anthropic `stop_reason` |
|---|---|---|
| `stop` | `stop` | `end_turn`, `stop_sequence` |
| `length` | `length` | `max_tokens` |
| `tool_calls` | `tool_calls` | `tool_use` |
| `content_filter` | `content_filter` | `refusal` |
| `paused` | `stop` | `pause_turn` |

The native interface always returns `native_finish_reason` as well.

#### Usage

| Canonical field | OpenAI-compatible source | Anthropic source |
|---|---|---|
| `input_tokens` (not cached) | `prompt_tokens` minus cached tokens | `input_tokens` |
| `cache_read_tokens` | `prompt_tokens_details.cached_tokens` | `cache_read_input_tokens` |
| `cache_write_tokens` | none | `cache_creation_input_tokens`, split by 5-minute and 1-hour lifetime |
| `output_tokens` | `completion_tokens` | `output_tokens` |
| `reasoning_tokens` (part of output) | `completion_tokens_details.reasoning_tokens` | not reported separately (to verify) |
| `server_tool_calls` | – | web-search request count in `usage` (to verify) |

The two conventions disagree: OpenAI's `prompt_tokens` includes cached tokens, Anthropic's `input_tokens` does not. The canonical form keeps the parts separate. On the OpenAI-compatible response the gateway reports `prompt_tokens` as the sum, with the cached part in `prompt_tokens_details.cached_tokens`.

### 8.5 Adapter contract

Each family has two halves. The **provider half** builds the provider request from the canonical request, decodes a response, decodes stream chunks into canonical events, extracts usage, maps errors, probes, and lists models. The **client half** does the reverse for callers speaking that dialect: it reads their request into the canonical model and writes responses, streams and errors in their shape. With both halves for both families, every interface reaches every provider. Proxy mode reuses only the usage extractor and the error classifier.

### 8.6 Conformance fixtures

Recorded request and response pairs per provider and feature (non-streaming, streaming, usage, reasoning, caching, images, tools, errors) run in CI against each adapter. Three uses:

- They close every "(to verify)" in this document before the code depends on it.
- A second set, recorded from the LiteLLM version PACOD runs today, pins the response shapes PACOD parses (section 18.2).
- The Anthropic set is the widest, and each case runs through all three interfaces: parallel tool calls, thinking across tool turns, a cache read on a second identical request, server tools, citations, documents, structured output, refusals, paused turns and token counting.

---

## 9. Interfaces

### 9.1 Data-plane endpoints

| Endpoint | Mode | Phase |
|---|---|---|
| `POST /v1/chat/completions` | managed, OpenAI-compatible | P1 |
| `POST /v1/audio/speech` | managed, OpenAI-compatible | P1 |
| `POST /v1/messages`, `POST /v1/messages/count_tokens` | managed, Anthropic-compatible | P1 |
| `/v1/messages/batches…`, `/v1/files…` | managed, Anthropic-compatible | P1 |
| `GET /v1/models` | managed; the shape follows the caller's dialect | P1 |
| `POST /gw/v1/chat` | managed, native | P1 |
| `POST /gw/v1/speech` | managed, native | P1 |
| `GET /gw/v1/models`, `GET /gw/v1/models/{ref}` | native | P1 |
| `GET /gw/v1/quota` | native | P1 |
| `ANY /proxy/{provider}/…` | proxy | P1 |
| `GET /healthz`, `GET /readyz` | – | P1 |
| `POST /v1/embeddings`, `POST /v1/responses` (stateless) | managed | P2 |
| `POST /gw/v1/usage` (book external costs) | native | P2 |

### 9.2 The native interface

The native interface is provider-neutral. Content is a list of typed blocks, reasoning and caching have one vocabulary, and everything that concerns the gateway rather than the model sits in a `gateway` object.

#### Request

```json
{
  "model": "@synthesizer",
  "system": [
    { "type": "text", "text": "You are a careful analyst.", "cache": { "ttl": "5m" } }
  ],
  "messages": [
    { "role": "user", "content": [
      { "type": "text", "text": "Compare the two offers." },
      { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "..." } }
    ] }
  ],
  "max_output_tokens": 4000,
  "reasoning": { "effort": "high", "summary": true },
  "stream": false,
  "gateway": {
    "end_user": "u_18342",
    "session_id": "s_77",
    "trace_id": "turn_9f3",
    "tags": { "purpose": "synthesizer", "site": "pacod" },
    "agent_id": "research-agent",
    "user_id": "staff_204",
    "task": "compare-offers",
    "complexity": "high",
    "timeout_ms": 180000
  }
}
```

| Field | Meaning |
|---|---|
| `model` | A repository model id or `@template`. Required. |
| `system` | A string or text blocks. A block may carry `cache`. |
| `messages` | Roles `user`, `assistant`, `tool`. Content is a string or blocks. |
| Block types | `text` (optionally with `citations`), `image`, `document`, `tool_call`, `tool_result`, `reasoning` (text plus an opaque signature, replayable only to the provider that produced it), `server_tool_call` and `server_tool_result` (provider-executed tools, opaque payload), and `provider` (a block only the serving provider understands, returned and accepted back unchanged). `audio` follows in phase 2. |
| `max_output_tokens`, `temperature`, `top_p`, `frequency_penalty`, `presence_penalty`, `stop` | As usual; subject to the template and the capability profile. |
| `reasoning` | `effort` (`none`, `low`, `medium`, `high`, `xhigh`, `max`), optional `budget_tokens`, `summary` (return reasoning text). Mapped to each provider's control. |
| `tools`, `tool_choice` | Function tools (`name`, `description`, `parameters`, optional `strict`) and provider tools (`provider`, `definition` passed verbatim). |
| `response_format` | Plain text, or a JSON schema the answer must follow. |
| `stream` | Server-sent events when true. |
| `provider_options` | A map keyed by adapter family for vendor-specific parameters. Applied only when that family serves the call. |
| `gateway` | Gateway parameters, below. |

**Gateway parameters.** Each can be sent in the `gateway` object or as a header, so they also work on the OpenAI-compatible endpoints.

| Parameter | Header | Meaning |
|---|---|---|
| `template` | `x-gw-template` | Alternative to `model: "@name"`. |
| `end_user` | `x-gw-end-user` | End-user id for attribution and end-user limits. |
| `session_id` | `x-gw-session` | Conversation id. |
| `trace_id` | `x-gw-trace` | Groups the calls of one unit of work. |
| `tags` | `x-gw-tags` (`k=v,k=v`) | Attribution tags. |
| `agent_id` | `x-gw-agent-id` | The agent that makes the call. Up to 200 characters. |
| `user_id` | `x-gw-user-id` | The platform user the agent acts for; not the end user. Up to 200 characters. |
| `task` | `x-gw-task` | What the call is for. Up to 64 characters. |
| `complexity` | `x-gw-complexity` | `low`, `medium` or `high`; anything else is `400 invalid_request`. |
| `timeout_ms` | `x-gw-timeout-ms` | Lower than or equal to the template's ceiling. |
| `retry.max_attempts` | `x-gw-retries` | Lower than or equal to the template's value. With a pool it caps the attempts of the whole call, across models: `1` means no retry and no failover. |
| `unsupported_params` | `x-gw-unsupported-params` | `drop` or `reject`. |
| `audit` | `x-gw-audit` | Record this call in full, where the workspace allows audit mode on request (section 14.6). |
| `cache.prompt` | `x-gw-prompt-cache` | `off`, `passthrough` (keep the caller's markers) or `auto` (the provider places the marker; Anthropic models). |
| `cache.response`, `idempotency_key` | – | Reserved for phase 2. |
| `fallback` | – | Not planned: fallback is the server's, through template pools and routing schemes (section 7.5). |

#### Response

```json
{
  "id": "gw_01JC8Z4T",
  "model": "anthropic/claude-sonnet-5-5",
  "content": [
    { "type": "reasoning", "text": "Summary of the reasoning." },
    { "type": "text", "text": "Offer B is cheaper over three years because ..." }
  ],
  "finish_reason": "stop",
  "native_finish_reason": "end_turn",
  "usage": {
    "input_tokens": 812,
    "cache_read_tokens": 6100,
    "cache_write_tokens": 0,
    "output_tokens": 420,
    "reasoning_tokens": 0,
    "source": "provider",
    "cost": { "amount": 0.007044, "currency": "USD", "price_version": 37 }
  },
  "gateway": {
    "request_id": "gw_01JC8Z4T",
    "workspace": "pacod-prod",
    "template": "synthesizer",
    "template_version": 4,
    "model_requested": "@synthesizer",
    "model_served": "anthropic/claude-sonnet-5-5",
    "provider": "anthropic",
    "attempts": 1,
    "routing": { "scheme": "by-complexity", "rule": "review", "source": "organisation" },
    "models_tried": [],
    "latency_ms": { "gateway_delay": 1.2, "pre": 0.9, "post": 0.3, "connect": 0, "retry": 0,
                    "upstream_first_byte": 640, "total": 5210 },
    "dropped_params": ["temperature"],
    "warnings": []
  }
}
```

**Stream events.** Each event is an SSE `event:` line with a JSON `data:` line.

| Event | Carries |
|---|---|
| `message.start` | Request id, `model_served`, template and version. |
| `content.start` | Block index and type. |
| `content.delta` | Block index and a delta: text, reasoning text, tool-call arguments or signature. |
| `content.stop` | Block index. |
| `message.stop` | `finish_reason`, `native_finish_reason`, `usage` with cost, the `gateway` block. |
| `error` | An error object; ends the stream. |
| `ping` | Keep-alive. |

Every stream ends with exactly one `message.stop` or `error`. Usage is always on `message.stop`.

**Errors.** One envelope for every gateway error:

```json
{ "error": { "type": "quota_exceeded", "message": "Daily token limit reached for end user u_18342.",
             "source": "gateway", "retryable": false, "request_id": "gw_01JC8Z4T",
             "details": { "limit": "end-user-daily-tokens", "resets_at": "2026-10-05T00:00:00Z" } } }
```

| HTTP | `type` | Source | Retryable | When |
|---|---|---|---|---|
| 400 | `invalid_request` | gateway | no | Malformed or contradictory request. |
| 400 | `unsupported_feature` | gateway | no | The feature cannot be delivered on this model or adapter. |
| 400 | `usage_required` | gateway | no | Proxy mode, policy `require`, stream without usage. |
| 400 | `template_provider_mismatch`, `template_violation` | gateway | no | Proxy mode: the template's model is at another provider, or a `check` template's rules are not met. |
| 401 | `invalid_key` | gateway | no | Unknown, expired or revoked key. |
| 402 | `quota_exceeded` | gateway | no | A token, request or spend limit for the window is used up. |
| 403 | `not_allowed` | gateway | no | Mode, model or template not permitted for this key. |
| 404 | `model_not_found`, `template_not_found` | gateway | no | |
| 410 | `model_retired` | gateway | no | Retired, and no successor may be followed. |
| 429 | `rate_limited` | gateway | yes | Per-minute throttle or concurrency cap; `Retry-After` is set. |
| 429 | `upstream_rate_limited` | upstream | yes | The provider's own limit, after the gateway's retries. |
| 502 | `upstream_error` | upstream | yes | Provider 5xx or connection failure, after retries. |
| 503 | `model_disabled`, `gateway_overloaded` | gateway | later | |
| 503 | `audit_unavailable` | gateway | later | Audit mode is required for the call and its record cannot be stored. |
| 504 | `upstream_timeout` | upstream | **no** | Never retried by the gateway; clients must not retry either. |

A used-up quota is `402`, not `429`, because SDKs and PACOD retry `429` and a spent quota does not recover by retrying.

**Response headers** on every managed and proxy response: `x-gw-request-id`, `x-gw-model-served`, `x-gw-attempts`, and where they apply `x-gw-dropped-params`, `x-gw-model-deprecated`, `x-gw-quota-remaining`, `x-gw-upstream-request-id` and, on non-streaming responses, `x-gw-cost`. `x-gw-delay-ms` carries the gateway delay of the call (section 14.2). A call a routing scheme decided carries `x-gw-route` (`scheme/rule`), and a call that moved on from a failing model `x-gw-failovers`; `x-gw-attempts` then counts the attempts on every model.

**Quota state.** `GET /gw/v1/quota?end_user=…` returns the limits that apply to the key (and end user), each with used, remaining and reset time. Applications use it to show users their allowance and to decide admission at their own boundaries.

**Speech.** `POST /gw/v1/speech` takes `model`, `input`, `voice`, `format` and `gateway`, and returns audio. Usage is counted in characters.

### 9.3 The OpenAI-compatible interface

`/v1/chat/completions` accepts the standard request and maps it onto the native model. In addition:

- A `gateway` object in the body (OpenAI SDKs send it through `extra_body`) or the `x-gw-*` headers.
- `model` may be `@template`.
- `reasoning_effort`, a `thinking` object and `cache_control` on content blocks are accepted and mapped.
- The response adds `usage.cost`, cached and reasoning token details, reasoning deltas, provider result blocks under `provider_specific_fields`, and the `x-gw-*` headers.
- Tools and JSON-schema output are translated for Anthropic models. Their thinking blocks are returned under `provider_specific_fields` on the assistant message and must be sent back with it in later turns.
- Gateway errors use OpenAI's error envelope with `code` set to the gateway error type.
- Unknown top-level fields are forwarded when the provider speaks the same dialect and are dropped or rejected, per policy, when the request is translated.

### 9.4 The Anthropic-compatible interface

Anthropic's SDK, and tools built on it, work against the gateway with two settings: the base URL is the gateway's root (the SDK appends `/v1/messages`), and the key is a gateway key, sent in `x-api-key` as usual.

| Endpoint | Behaviour |
|---|---|
| `POST /v1/messages` | The Messages API, streaming or not. Section 8.3 describes what is governed and what is forwarded. |
| `POST /v1/messages/count_tokens` | Resolved like a message, forwarded to Anthropic, free. |
| `GET /v1/models`, `GET /v1/models/{id}` | Answered in Anthropic's shape when the request carries `anthropic-version`. Lists what the key may use. |
| `/v1/messages/batches…` | Create, retrieve, list, cancel, delete and results, scoped to the workspace. |
| `/v1/files…` | Upload, list, read and delete, scoped to the workspace. |

**How `model` is resolved.** In this order: a repository model id; `@template`; the provider's own model id (for example `claude-sonnet-5-5`), if exactly one model granted to the workspace has it. The third rule lets tools that hard-code Anthropic's model names work unchanged. It applies on the OpenAI-compatible interface as well.

**Gateway parameters** go in the `x-gw-*` headers or in a `gateway` object in the body, which is removed before forwarding. `metadata.user_id` is used as the end user when no other is given.

**Responses.** For an Anthropic model the body and the stream events are Anthropic's own, unchanged. The gateway adds only its `x-gw-*` response headers, including `x-gw-cost` on non-streaming responses. The cost of a streamed call is available by request id from the request log. For a model of another provider the gateway builds the response and the event stream in Anthropic's shape.

**Errors** use Anthropic's envelope and type names, so the SDK's exception classes work. The HTTP status is the same as in section 9.2, and `x-gw-error-type` carries the gateway's own type.

| Gateway error | HTTP | Anthropic `error.type` |
|---|---|---|
| `invalid_request`, `unsupported_feature` | 400 | `invalid_request_error` |
| `invalid_key` | 401 | `authentication_error` |
| `quota_exceeded` | 402 | `billing_error` |
| `not_allowed` | 403 | `permission_error` |
| `model_not_found`, `template_not_found`, `model_retired` | 404, 410 | `not_found_error` |
| `rate_limited`, `upstream_rate_limited` | 429 | `rate_limit_error` |
| `upstream_error`, `model_disabled`, `gateway_overloaded` | 502, 503 | `api_error` |
| `upstream_timeout` | 504 | `api_error`, marked non-retryable |

Anthropic's own `overloaded_error` (529) is passed through as it is. Anthropic's SDK retries timeouts by default, so the non-retryable marker of section 4.4 matters here.

### 9.5 Proxy endpoints

| Client | Base URL to configure |
|---|---|
| OpenAI SDK | `https://<gateway>/proxy/openai/v1` |
| Anthropic SDK | `https://<gateway>/proxy/anthropic` |
| Gemini native | `https://<gateway>/proxy/gemini/v1beta/models/{model}:generateContent` |
| Ollama native | `https://<gateway>/proxy/ollama/api/chat` |
| GLM, DeepSeek, Kimi | `https://<gateway>/proxy/{glm,deepseek,kimi}/…` |

`{provider}` is the provider's slug in the repository. The `model` in the body is the provider's own id, or `@template` (section 7.4). The `x-gw-*` headers of section 9.2 work in proxy mode as well.

Errors raised by the gateway itself in proxy mode (an invalid key, a used-up quota, a template rule) are returned in the provider's error envelope for the OpenAI and Anthropic dialects, so provider SDKs handle them normally, and in the gateway's own envelope otherwise. `x-gw-error-type` always carries the gateway's type.

---

## 10. Management API

### 10.1 Principles

- Base path `/api/v1` on the management listener. JSON.
- **Everything the UI does goes through it.** An OpenAPI 3.1 document is generated from the handlers and served at `/api/v1/openapi.json`. The UI's TypeScript client is generated from that document in the build, and CI fails when they diverge.
- Authentication by login session or bearer admin token; authorisation by role and by organisation and workspace scope.
- Every mutation is validated, written together with its audit entry and a configuration-version increment in one transaction, applied locally, and answered with the new version and how many instances have applied it.
- Mutations that can hurt support `?dry_run=true`. Updates use a version field for optimistic concurrency.
- Lists are cursor-paginated, filterable and sortable.
- Secrets are write-only. Reads return a fingerprint.
- Model ids contain `/`, so resources are addressed by opaque ids (`mdl_…`, `tpl_…`) in paths; the readable id is a field.
- `GET /api/v1/events` is a server-sent stream of configuration changes, job progress and health transitions, for live UI updates.

### 10.2 Resource map

| Area | Resources under `/api/v1` | Notes |
|---|---|---|
| Providers | `providers`, `providers/{id}/probe` | Create, update, disable, test. |
| Credentials | `providers/{id}/credentials`, `orgs/{org}/credentials`, `…/workspaces/{ws}/credentials` | Add, rotate, disable. Write-only secret. |
| Models | `models`, `models/{id}/transition`, `models/{id}/probe`, `models/{id}/impact` | Life-cycle transitions with dry run. |
| Capability profiles | `capability-profiles` | Presets and per-model overrides. |
| Discovery | `discovery/runs`, `discovery/proposals` | Accept a proposal as a draft, or dismiss it. |
| Prices | `prices`, `prices/history`, `prices/import`, `price-sources`, `price-job`, `price-job/runs`, `price-proposals` | Section 13. |
| Organisations | `orgs`, `orgs/{org}/grants`, `orgs/{org}/limits`, `orgs/{org}/members` | |
| Workspaces | `orgs/{org}/workspaces`, `…/{ws}/grants`, `…/{ws}/settings`, `…/{ws}/tag-keys` | |
| Templates | `…/{ws}/templates`, `templates/{id}/versions`, `templates/{id}/rollback`, `templates/{id}/clone` | |
| API keys | `…/{ws}/keys`, `keys/{id}/rotate`, `keys/{id}/revoke` | Secret returned once. |
| Limits | `…/{ws}/limits`, `…/{ws}/limits/state`, `…/{ws}/end-users/{id}/limits` | Definitions, live state, per-end-user overrides and reset. |
| Access | `users`, `role-bindings`, `admin-tokens`, `session` | |
| Analytics | `analytics/overview`, `analytics/usage`, `analytics/breakdown`, `analytics/traces/{trace_id}`, `analytics/end-users`, `exports` | Forwarded to the analytics service (section 10.3). |
| Requests | `requests`, `requests/{id}`, `requests/{id}/audit`, `errors` | Request log with the timing of each call, failures, and the full record of audited calls. |
| Provider objects | `…/{ws}/files`, `…/{ws}/batches` | Files and batches created through the gateway: owner, status, cost. |
| Monitoring | `monitoring/live`, `monitoring/delay`, `monitoring/health`, `monitoring/alerts` | Live counters from the core, gateway delay percentiles and slowest calls, provider and model health, open alerts. |
| Playground | `playground/chat`, `playground/transform` | A test call as a workspace; a dry run that shows the exact provider request. |
| System | `system/status`, `system/config-versions`, `settings`, `audit-log` | Instances and applied versions, global settings, audit trail. |
| Internal | `internal/jobs/lease`, `internal/jobs/{id}/result` | Used by the analytics service with a service token. |

### 10.3 The analytics facade

The UI asks the core for analytics. The core checks the caller's role, replaces any organisation or workspace filter with the scope the caller is entitled to, forwards the query to the analytics service and streams the answer back. The analytics service runs every ClickHouse query. The core never does, so heavy aggregation stays out of the process that holds the provider keys and carries the traffic.

If the analytics service is down, these endpoints answer `503` with a clear error; nothing else is affected.

---

## 11. Configuration and hot reload

### 11.1 Where configuration lives

| Place | Content |
|---|---|
| Bootstrap file and environment | Listen addresses, role, database URL, analytics URL, spool directory, the reference to the key-encryption key and the HMAC pepper, the first admin account. Nothing else. |
| PostgreSQL | Everything in sections 5 to 7, 12 and 13, plus the audit log and limit counters. |
| Memory | The snapshot built from PostgreSQL, and fast-changing state. |

Providers and keys no longer appear in the TOML file or in `.env`.

### 11.2 How a change reaches the request path

1. An admin calls the management API.
2. The core validates the change, then writes the row, the audit entry and a new `config_version` in one transaction.
3. The same instance builds a new snapshot off the request path and swaps it in atomically. Requests already running finish on the old snapshot.
4. The API answers with the new version.
5. Every other instance polls the version about once a second, loads the changes since its own version (or everything, if there is a gap) and swaps its snapshot.
6. Each instance records the version it has applied. The API and UI can show "applied on 3 of 3 instances".

Polling a single row is cheap and cannot miss a change, which is the weakness of relying on PostgreSQL notifications alone. Notifications may be added as a wake-up hint; correctness never depends on them.

**The snapshot** contains providers, decrypted credentials, models with capability profiles, grants, templates, the key index, limit definitions, current prices and settings. **Fast state** stays outside it and is updated in place: key revocations, model and provider `disabled` flags, health and counters.

### 11.3 What "on the fly" means per change

| Change | Effective | Note |
|---|---|---|
| Add or rotate a provider key | Next request | The credential is attached per request, so open connections are unaffected. The old key stays valid during a grace period. |
| Change a provider's base URL | Next request | A new connection pool is created; the old one drains. |
| Add, edit or transition a model | Next request | Activation runs a probe first. |
| Edit a capability profile | Next request | |
| Create or edit a template | Next request | A new version; running calls keep theirs. |
| Create a gateway key | Next request | |
| Revoke a gateway key | ≤ 2 s on all instances | Fast state. |
| Change a limit | Next request | Counters are kept. |
| Switch audit mode on or off for a workspace or a template | Next request | An optional end time switches it off again. |
| Change a price | Next settled call | History is not re-rated. |

No change in this table needs a restart. A restart is needed only for bootstrap values.

### 11.4 Secrets

- Provider credentials are encrypted with AES-256-GCM under a per-secret data key, which is itself encrypted with a key-encryption key. That key comes from the environment or a file in the first release and from a KMS or Vault in phase 2. A key id on each record allows rotating it.
- The API accepts a secret and never returns it. Plaintext exists only in the snapshot in memory and is never logged.
- Rotation adds a credential version and makes it current. The previous one is kept as `retiring` for a grace period, then disabled.
- Several active credentials per provider, with rotation on 429, is a phase 2 item.

### 11.5 When parts are down

| Down | Effect |
|---|---|
| PostgreSQL | Traffic continues on the last snapshot. Counter flushes are buffered. The management API cannot write. |
| Analytics or ClickHouse | Traffic continues. Events accumulate in the spool. Audit-mode records accumulate in their own spool; calls that require a record are refused once it is full. Analytics endpoints answer 503. Batch jobs wait. |
| A core instance | The others carry on. Its spool is shipped when it returns. |

A configuration export and import (JSON) supports backup and moving a setup between environments.

---

## 12. Quotas, budgets and rate limits

### 12.1 Limit definition

| Field | Values |
|---|---|
| Scope | organisation, workspace, key, end user |
| Metric | tokens (all or by type), requests, spend |
| Window | per minute (sliding), hour, day, week, month (calendar-aligned in the workspace's time zone), lifetime |
| Amount | a number; `0` means nothing is allowed |
| Enforcement | `request` (the gateway rejects) or `advisory` (the gateway counts and reports) |
| Filter | optional: models, templates, tags |
| Alert thresholds | for example 50, 80 and 100 % |

All limits that apply to a request must pass. End-user limits are defined once per workspace as a default for every end user, with per-end-user overrides (a different amount, or exempt) and a reset operation.

### 12.2 Admission and settlement

- **Admission** happens before the request is forwarded: the throttle, the concurrency cap, and a check that no enforced limit is already used up. A rejection is `429 rate_limited` or `402 quota_exceeded`.
- **Settlement** happens when the call ends: actual tokens and cost are added to every matching counter.
- A limit can therefore be overshot by the calls that were in flight when it was reached. Reserving an estimate up front and cutting a stream when a budget runs out are phase 2 items. The first release never cuts a running stream.

### 12.3 Advisory limits

PACOD checks a user's quota once per chat turn and charges the turn's tokens at the end. A turn is several LLM calls, and rejecting one in the middle would break it. An advisory end-user limit supports this: the gateway counts every call carrying that `end_user`, exposes the state on `GET /gw/v1/quota`, and leaves the decision to the application at the start of its next turn. A limit's filter selects which calls count, for example only those with user-facing `purpose` tags.

### 12.4 Counters

- Counters are atomics in memory, per instance.
- Each instance flushes its deltas to PostgreSQL every second and reads the combined totals back. Counters survive restarts.
- With several instances, enforcement lags by at most that interval. The overshoot bound is the in-flight calls plus one second of traffic.
- A strict shared counter store (Valkey) is an optional phase 2 backend for installations that need tighter bounds.

### 12.5 What this gives PACOD

| PACOD today | In the gateway |
|---|---|
| Token quota per user, period hour/day/week/month | End-user limit, metric tokens, the same windows |
| Request quota per user | End-user limit, metric requests |
| Per-minute throttle per user | End-user limit, per-minute window |
| Per-user overrides and reset | End-user overrides and reset |
| User sees own state | `GET /gw/v1/quota` |
| No spend cap, no spend alert | Workspace or organisation spend limit with alert thresholds |
| Worker spend uncapped | A limit on the worker's key |

---

## 13. Cost model and price tracker

### 13.1 Prices

A price belongs to a model and has versions.

| Field | Content |
|---|---|
| Rates per 1M tokens | input, output, cache read, cache write (5-minute and 1-hour), reasoning if priced differently |
| Rates per unit | per character (speech), per request, per server-tool call (for example a web search) |
| Variants | Separate rates for batch processing and for fast mode, where a provider offers them. The variant is chosen from what the response reports. |
| `currency` | The currency the provider publishes. Reports use one reporting currency per installation; other currencies are converted at an admin-set rate. |
| `effective_from`, `version` | When it applies. |
| `source` | `manual`, `feed`, `extracted`, `import`, `seed` |
| `status` | `active`, `proposed`, `superseded` |
| `evidence` | Source URL and the text the figures were taken from. |

### 13.2 Cost rules

- Cost = Σ (token count × rate) + Σ (unit count × unit rate), computed at settlement and stored on the event with the price version.
- **Not retroactive.** A price change applies to calls settled afterwards.
- **Cache-aware.** Cached input is charged at the cache-read rate, cache writes at their own rate. PACOD today charges cached tokens at the full input rate and overstates cost.
- **No default price.** A model cannot be activated without a price or an explicit "free". If a price is missing at settlement, cost is empty, the event is flagged `price_missing` and an alert fires.
- Local models (Ollama) are "free" or carry an admin-set rate.

Worked example, with Anthropic's published rates for Claude Sonnet 5.5 (2.00 input, 10.00 output, 0.20 cache read, USD per 1M): 812 input, 6 100 cached and 420 output tokens cost 0.001624 + 0.001220 + 0.004200 = 0.007044 USD. Charged without the cache rate, the same call would be 0.018024 USD.

### 13.3 Price tracker

"Provisioned models" means every model in the repository that is active or deprecated, plus drafts that are waiting for a price.

**Configuration** (through the management API and the Pricing pages):

| Setting | Meaning |
|---|---|
| Enabled, schedule | A cron expression and time zone. Default daily. |
| Sources per provider | An ordered list, below. |
| Auto-apply bound | The largest relative change applied without approval, for example 30 %. |
| Extraction template | The template in the `_system` workspace used for LLM extraction. |
| Stale threshold | Age in days after which a price raises an alert. |

#### Source types

| Type | How |
|---|---|
| `feed` | A structured, machine-readable price list. Which public lists are trustworthy for which provider is to verify; PACOD found that LiteLLM's public list did not cover Anthropic, OpenAI and GLM adequately. |
| `page` | A curated pricing-page URL per provider. The page is fetched and an LLM extracts the rates for the requested model ids, returning the text they came from. |
| `manual` | Entered by an admin. Never overwritten by the job. |
| `import` | A price file, for installations with no outbound access. |

#### A run

1. The analytics service takes the job lease from the core, so only one instance runs it, and receives the configuration, the model list and the current prices.
2. For each provider it tries the sources in order until one yields rates.
3. It validates the candidates: units, positive values, plausible ratios.
4. It posts the candidates to the core. The core decides per model:
   - the current price is `manual` → skipped;
   - no change → unchanged;
   - a change within the bound → a new version, applied;
   - a change beyond the bound, or a first price → a **proposal** for an admin to approve or reject;
   - nothing found → `no_price`.
5. The core stores the run with per-model results and publishes it on the event stream.

Extraction calls go through the gateway's own data plane with a key of the `_system` workspace and the tag `purpose=price_fetch`, so the job's cost is accounted like any other usage.

**Triggers:** the schedule, "run now", and model activation.

The job runs in the analytics service and not in the core because it fetches external pages: the process that holds every provider key should not be the one parsing arbitrary HTML.

---

## 14. Monitoring, analytics and observability

Three questions, three mechanisms. **"Is it healthy right now?"** is answered by metrics, traces and alerts. **"Who used what, what did it cost, and how much time did the gateway add?"** is answered by usage events and the reports built on them. **"What exactly was sent and answered?"** is answered by audit mode. All three are reachable through the management API.

### 14.1 Usage event

One event per logical call, in both modes. It extends the scaffold's `UsageEvent`.

| Group | Fields |
|---|---|
| Identity | `id` (UUIDv7), `timestamp`, `instance_id` |
| Scope | `organisation_id`, `workspace_id`, `key_id` |
| Attribution | `end_user`, `session_id`, `trace_id`, `tags`, `agent_id`, `user_id`, `task`, `complexity` |
| Routing | `mode`, `interface`, `provider`, `model_requested`, `model_served`, `template`, `template_version` |
| Outcome | `status` (ok, error, timeout, cancelled), `http_status`, `error_type`, `error_message` (truncated), `finish_reason`, `attempts`, `stream` |
| Timing | `gateway_delay_ms` and the stages behind it: `receive_ms`, `pre_ms`, `connect_ms`, `retry_ms`, `upstream_first_byte_ms`, `upstream_stream_ms`, `post_ms`, `relay_max_ms`, `settle_ms`, `total_ms` (section 14.2) |
| Usage | `input_tokens`, `cache_read_tokens`, `cache_write_tokens`, `output_tokens`, `reasoning_tokens`, `characters`, `server_tool_calls`, `usage_source` |
| Cost | `cost`, `currency`, `price_version`, `price_variant`, `price_missing` |
| Other | `dropped_params`, `audit_id` (set when audit mode recorded the call) |

### 14.2 Gateway delay

For every call, in both modes, the core measures how much time the gateway itself added and stores it on the usage event. This is always on. It does not need audit mode.

Times are taken with a monotonic clock at microsecond resolution and stored in milliseconds.

| Field | What it measures | Gateway delay |
|---|---|---|
| `receive_ms` | Reading the request from the client. | no: client and network |
| `pre_ms` | From the complete request to the start of the provider call: authentication, template, limits, translation. | **yes** |
| `connect_ms` | Waiting for a pooled connection to the provider, or opening a new one. Zero when a connection is reused. | shown separately |
| `retry_ms` | Failed attempts and the waits between them. | shown separately |
| `upstream_first_byte_ms` | From the request sent to the provider's first byte. | no: provider |
| `upstream_stream_ms` | From the provider's first byte to its last. | no: provider |
| `post_ms` | The gateway's work on the answer. For a stream: from the provider's first byte to the first byte sent to the client. For a single response: from the provider's last byte to the last byte sent. | **yes** |
| `relay_max_ms` | Streams only: the longest time any chunk spent inside the gateway. | reported with it |
| `settle_ms` | Accounting after the response has been sent. The client never waits for it. | no |
| `total_ms` | From the first request byte to the last response byte. | – |

**`gateway_delay_ms` = `pre_ms` + `post_ms`.** It is the time by which the gateway delayed the answer, compared with the same call made to the provider directly. `connect_ms` and `retry_ms` also lengthen a call, but they depend on the provider and the network, so they are stored and shown next to the gateway delay and are not added into it.

The latency targets of section 4.5 are targets for `gateway_delay_ms`.

#### Where the delay is visible

| Place | What |
|---|---|
| Every response | The `x-gw-delay-ms` header. For a stream it is sent with the first byte, which matches the definition. The native interface also returns the stages in its `gateway` block. |
| Usage event | All fields above, for every call. |
| Management API | `requests/{id}` returns the stages of one call. `monitoring/delay` returns the median, the 95th and 99th percentile and the maximum over time, grouped by mode, provider, model, template, workspace or instance, and lists the slowest calls. `monitoring/live` returns the current values from the core's own histograms. |
| UI | Requests: a stage bar for each call. Monitoring: gateway delay over time against the targets, the split into stages, and the slowest calls. Dashboard: a tile with the current median and 99th percentile. |
| Prometheus and Grafana | The histogram `gw_delay_seconds` by mode, provider and stage, a panel on the Gateway internals dashboard, and an alert when the 99th percentile stays above the target. |

### 14.3 Delivery without loss

```text
request task ─► bounded channel ─► spool writer ─► shipper ─► analytics /ingest ─► ClickHouse
                                   append-only     batches     200 only after the insert
                                   CRC per record  checkpoint advances on 200
                                   fsync every 100 ms
```

- Delivery is at least once. ClickHouse removes duplicates by event id.
- A process crash loses at most the unsynced window. Only loss of the instance's disk loses more.
- The spool has a size cap. When analytics has been unreachable long enough to fill it, the policy decides: `keep_serving` (default; new events are dropped, counted in a metric, and an alert fires) or `fail_closed` (new requests are refused) for installations that bill from the data.
- Limits do not depend on this path; counters are settled in the core.

### 14.4 Storage

| ClickHouse object | Content | Retention |
|---|---|---|
| `usage_events` | One row per event; partitioned by month; duplicates removed by id | Configurable, default unlimited |
| `message_audit` | The full record of each audited call (section 14.6) | Per workspace, default 14 days |
| Hourly rollup | By organisation, workspace, key, provider, model, template, status | As events |
| Daily rollups | By end user; by tag key and value | As events |

Failures are events with a status other than `ok`. A separate error table is not needed; the "errors" view is a filter.

### 14.5 Reports

Served through the core (section 10.3):

| Report | Answers |
|---|---|
| Overview | Spend, tokens and requests for the current hour, day, week and month. |
| Usage over time | A time series by hour, day, week or month, filtered and grouped by provider, model, template, key, tag, end user, agent, user, task or complexity. |
| Breakdown | Top models, templates, keys, tags, end users, agents, users, tasks and complexities for a range. |
| Per trace | Every call of one `trace_id` with tokens, cost and gateway delay, and their totals. This is PACOD's per-turn view. |
| End users | A list with consumption, and a drill-down for one end user. |
| Errors | Failures by provider, model, type and time. |
| Gateway delay | Percentiles of `gateway_delay_ms` and its stages over time, by mode, provider, model, template, workspace, instance, agent, user, task or complexity; the slowest calls. |
| Accuracy | The share of spend with `usage_source` other than `provider`, and events with `price_missing`. |
| Ranking | Providers or models of one kind (chat or speech) ranked on success rate, time to first byte, output time per token and cost, each on its own and as one score weighed by a preset. |
| Export | CSV of any of the above. |

### 14.6 Audit mode

Audit mode records calls in full: every message in both directions, with everything the gateway knows about the call. One mechanism serves two purposes. For **debugging**, it is switched on for a template or a workspace for a while. For **auditing**, it stays on and the records are retained.

It works in proxy mode and in managed mode. It is the gateway's equivalent of the LLM steps in PACOD's Debug Console. It is separate from the audit log of section 14.10, which records what administrators change.

#### Switching it on

| Level | Setting | Meaning |
|---|---|---|
| Workspace | `audit.mode`: `off` (default), `on`, `on_request` | Applies to every call in the workspace. With `on_request`, only calls that ask for it are recorded. |
| Template | `audit`: `inherit` (default), `on`, `off` | Overrides the workspace for calls that use the template. |
| Request | `gateway.audit`, or the `x-gw-audit` header | Honoured when the workspace mode is `on_request`. |
| Organisation, platform | `forbid` or `force` | A higher level can rule audit mode out, for privacy, or make it mandatory, for compliance. |

The settings are changed through the management API and the UI and take effect with the next request. Each change is written to the audit log.

| Option | Meaning |
|---|---|
| `until` | An optional end time, after which audit mode switches itself off. Suits a debugging session. |
| `sample` | The share of calls recorded. 100 % by default. |
| `retention_days` | How long the records are kept. |
| `attachments` | `full` (default) stores images, documents and audio as sent. `reference` stores their type, size and hash. |
| `guarantee` | `best_effort` (default) or `required`, see below. |

#### What a record contains

| Part | Content |
|---|---|
| Client request | Method, path, query, headers, and the body exactly as received. |
| Provider request | For every attempt: URL, headers, and the body exactly as sent, after the template, the parameter merge and any translation. |
| Provider response | For every attempt: status, headers, and the body exactly as received. For a stream, every chunk with its arrival time. |
| Client response | Status, headers, and the body or stream exactly as sent to the client, with the send time of each chunk. |
| Resolution | Key, workspace, organisation, end user, session, trace and tags; template and version; where each parameter value came from; what the capability filter dropped; the model requested and the model served; provider and credential fingerprint; the limits checked and their state. |
| Timeline | The time stamps behind the stages of section 14.2. |
| Outcome | Usage, cost and price version, finish reason, and for a failure the provider's complete error. |

Only credentials are left out: the gateway key is replaced by its id, and provider keys and authorization headers are removed in both directions. Nothing else is removed or summarised. A record above the configured size limit is stored up to the limit and marked as truncated, never cut silently.

Where two parts are byte-identical, the body is stored once. That is the normal case in proxy mode without a template.

#### Storage and delivery

- Records travel like usage events (section 14.3), in a spool of their own with its own size cap, so large payloads never crowd out usage events.
- They are stored in ClickHouse and linked to the usage event by `audit_id`. Rows are ordered by workspace, session and time, so compression absorbs the repetition between the turns of a conversation, where each turn sends the history again. Attachments are stored once per content hash.
- No step on the request path waits for storage. Audit mode costs memory, because bodies are held until written, and disk. The gateway delay of an audited call is measured like any other, so the cost is visible. The concurrency targets of section 4.5 are stated for calls without audit mode.
- With `guarantee: required`, a call is admitted only if the audit spool can take its record; otherwise it is refused with `503 audit_unavailable`. With `best_effort`, the call proceeds, the missing record is counted in a metric and an alert fires.

#### Reading the records

- **UI.** The Requests page shows, for each audited call, the four bodies, a comparison of the request as received with the request as sent to the provider (what the gateway changed), the timeline, and the outcome. For a trace it shows the calls in sequence.
- **API.** `requests/{id}/audit` returns the complete record. `requests` can be searched by request id, trace, session, end user, template, model, status and time. A trace or a session can be exported as JSON lines.
- **Access.** Workspace admins and auditors only. Every read is written to the audit log.
- **Retention.** A job deletes expired records. Records of one end user or one session can be deleted on request.

### 14.7 Metrics

Exposed by the core at `/metrics`.

| Series | Labels |
|---|---|
| `gw_requests_total` | organisation, workspace, provider, model, mode, outcome, complexity |
| `gw_retries_total` | provider, model |
| `gw_tokens_total` | organisation, workspace, provider, model, kind |
| `gw_cost_total` | organisation, workspace, provider, model, currency |
| `gw_request_duration_seconds` (histogram) | provider, mode, complexity |
| `gw_first_byte_seconds` (histogram) | provider, mode |
| `gw_delay_seconds` (histogram of the gateway delay) | provider, mode, stage |
| `gw_streams_in_flight` | mode |
| `gw_rejections_total` | reason |
| `gw_limit_used_ratio` | organisation, workspace, limit (organisation and workspace limits only) |
| `gw_upstream_health` | provider, model |
| `gw_config_version`, `gw_config_apply_lag_seconds` | instance |
| `gw_spool_bytes`, `gw_events_dropped_total` | instance |
| `gw_audit_spool_bytes`, `gw_audit_records_dropped_total` | instance |
| `gw_price_age_days` | model |

There are no key or end-user labels, and no agent, user or task labels: their values are open-ended and are reported from the usage events instead. `complexity` is `none` on calls that declare none. Because every call passes through the gateway, these series include worker traffic, which PACOD's own Prometheus series miss today.

### 14.8 Traces and logs

- OpenTelemetry traces with the GenAI semantic conventions: a span per request and a child span per upstream attempt, carrying the provider's request id. Prompt and response content is never put in spans.
- Structured JSON logs. No bodies and no authorization headers.

### 14.9 Dashboards and alerts

Grafana dashboards shipped with the stack: Overview, Providers and models, Workspaces, Gateway internals.

| Alert | Condition |
|---|---|
| Error rate high | More than 10 % failed calls per provider and model over 15 minutes, above a minimum volume |
| Timeouts spike | More than a handful of timeouts in 15 minutes |
| All calls failing | Calls attempted and none succeeded in 15 minutes |
| Rejection spike | Any rejection reason above one per second for 10 minutes |
| Limit nearly used | `gw_limit_used_ratio` above a threshold (covers spend caps) |
| Price missing or stale | An event with `price_missing`, or a price older than the stale threshold |
| Spool backlog | Spool size growing, or events dropped |
| Gateway delay high | The 99th percentile of `gw_delay_seconds` above the target of section 4.5 for 10 minutes |
| Audit records lost | An audit record dropped, or the audit spool nearly full |
| Configuration lag | An instance behind the current version for more than 30 seconds |

The first four mirror PACOD's existing rules. Open alerts are also listed at `monitoring/alerts`, so the UI does not need Prometheus access. Webhook and e-mail delivery follow in phase 2; Alertmanager covers it until then.

### 14.10 Audit log

The audit log records what administrators do; audit mode (section 14.6) records what applications send and receive.

Every management mutation and every read of an audit-mode record is written to PostgreSQL: who, what, on which object, a hash of the state before and after, and the configuration version. It is exposed at `audit-log`.

---

## 15. UI

### 15.1 Principles

- SvelteKit with Svelte 5 and the Node adapter, as scaffolded.
- One backend: the core's management API, through a client generated from its OpenAPI document.
- Calls are made on the SvelteKit server side with the user's session; the browser does not hold an admin token.
- An organisation and workspace switcher in the shell. Navigation depends on the role.
- Live updates from the management event stream.
- Forms show only what the API reports as valid, for example the parameters a model's capability profile accepts.

### 15.2 Pages

| Area | Page | Shows and does |
|---|---|---|
| Home | Dashboard | Spend, requests and errors for the selected scope; gateway delay; provider health; open alerts. |
| Platform | Providers | Providers, base URLs, credentials (add, rotate), probe. |
| | Models | The repository with state; life-cycle actions with an impact preview; capability editor; discovery proposals. |
| | Pricing | Current prices and history; proposals to approve; sources; schedule; run now; past runs. |
| | Organisations | Create and manage organisations and their grants. |
| | Access | Users, roles, admin tokens. |
| | System | Instances and applied configuration version; settings; audit log. |
| Organisation | Workspaces | Create and manage workspaces; grants; caps; members; own credentials. |
| Workspace | Models | Granted models with state and price. |
| | Templates | Editor with valid parameters only; versions and rollback; audit-mode switch; test call. |
| | API keys | Create, rotate, revoke. |
| | Limits | Definitions, live state, end-user overrides and reset. |
| | Settings | Audit mode with its options, unsupported-parameter policy, allowed provider tools, tag keys, retention. |
| Insight | Usage and costs | Overview and time series with grouping and filters; export. |
| | Analytics | By model, template, tag, key and end user; errors; accuracy. |
| | Ranking | Providers and models side by side: the leader per metric, the overall score per preset, and every metric with its rank. |
| | Requests | Request log with a stage bar per call; one request or trace in detail; for audited calls the complete requests and responses, and what the gateway changed; files and batches created through the gateway. |
| | Playground | A test call; the transform preview. |
| | Monitoring | Live counters; gateway delay over time against the targets, by stage, and the slowest calls; health; alerts; link to Grafana. |

---

## 16. Security

The gateway holds every provider key and sees every prompt, so the threat model comes before code. The first version of it:

| Threat | Control |
|---|---|
| Provider keys stolen from storage | Envelope encryption; the key-encryption key is kept apart from the database and its backups. |
| Provider keys leaked through the API or logs | Write-only API; fingerprints on read; no secrets in logs, traces or errors. |
| Guessing or flooding gateway keys | High-entropy keys; constant-time HMAC lookup with no slow hash; failed authentication is rate-limited per source. |
| Management API exposed | Separate listener, authenticated from the first release, not served by `--role gateway`. Local passwords use a slow hash; that is appropriate there because logins are rare. |
| Server-side request forgery through admin-supplied URLs | Provider base URLs and price sources are checked against an egress allow-list; link-local, metadata and internal ranges are blocked unless explicitly allowed (a private Ollama host needs that). Price pages are fetched by the analytics service, which holds no provider keys. |
| One workspace reading another's data | The workspace comes from the key, never from the request. The core injects the scope into every analytics query. |
| One workspace using another's files or batches at a provider | Provider credentials are usually shared, so the gateway records who created each file and batch id, filters lists, and rejects references to ids the workspace does not own. |
| A caller making the provider reach servers of its choosing | Provider-executed tools (web fetch, code execution, the MCP connector) are allowed per workspace by type. Tool types not on the list are rejected before forwarding. |
| Disagreement between gateway and provider about a request | Strict JSON decoding in managed mode; proxy mode rejects a body that repeats any top-level key the gateway reads or writes. |
| Calling a model server directly | Local model servers accept traffic only from the gateway (network policy or compose network). |
| Prompts in telemetry | Content is absent from logs, traces and usage events. |
| Stored prompts and answers (audit mode) | Off by default; an organisation can forbid it. Credentials are never stored. Only workspace admins and auditors can read records, every read is logged, and records expire. |
| Compromised dependency or image | Locked dependencies, vulnerability and licence checks in CI, a software bill of materials, signed images, a minimal base image, pinned image tags in `deploy/`. |
| Abuse by an administrator | Role separation and an audit log of every mutation and every read of an audit-mode record. |

A security policy with a disclosure contact is part of the first release.

---

## 17. Deployment and operations

| Setup | Parts | What works |
|---|---|---|
| Minimal | Core, PostgreSQL | Proxy and managed mode, management API, limits, metrics including the gateway delay. No reports, no audit mode, no price tracker. |
| Standard | Plus analytics, ClickHouse, OTel collector, Prometheus, Grafana | Everything in this concept. This is the compose stack. |
| Scaled | Several `gateway` instances behind a load balancer, on one host or on several machines, one or two `admin` instances with the one console, shared PostgreSQL | The same, with the bounds of sections 11.2 and 12.4. The console shows every instance from figures the instances publish in PostgreSQL. |

| Dependency | Used for | Required |
|---|---|---|
| PostgreSQL | Configuration, encrypted secrets, audit log, limit counters, job leases | Yes |
| Local disk | Event spool | Yes |
| ClickHouse | Usage events, audit-mode records, rollups | For analytics and audit mode |
| Prometheus, Grafana, OTel collector | Metrics, dashboards, alert rules, traces | Optional |

### Operations

- `/healthz` reports that the process is alive; `/readyz` reports that a snapshot is loaded and the spool is writable. Compose and Kubernetes wait on `/readyz`, which closes the gap PACOD has today, where a proxy that failed to start is noticed only at the first call.
- Graceful shutdown: report not ready while still serving, so that the load balancer moves on; then stop accepting, let streams finish within a limit, flush the spool.
- Backups: PostgreSQL, with the key-encryption key stored separately; ClickHouse on its own schedule. The spool is transient.
- Releases are versioned images with pinned tags. PostgreSQL and ClickHouse schemas are migrated on start. An instance one version behind must still read the current configuration.
- The core makes no outbound calls except to configured providers. Only the price tracker and discovery jobs reach other hosts, and both can be switched off.

Docker Compose is the packaging for the first release; a Helm chart follows in phase 2.

---

## 18. PACOD coverage and migration

### 18.1 Coverage matrix

Sections refer to [LLM_USAGE.md](LLM_USAGE.md). Everything in the "gateway" column is in the first release unless marked.

| § | PACOD today | In the gateway | Stays in PACOD |
|---|---|---|---|
| 2.1 | One proxy, one master key shared by all services | One API key per service in a PACOD workspace | – |
| 2.2 | `model_list`, aliases, `os.environ` key references, generated include file | Model repository (id and upstream id), credentials through the API | – |
| 2.3 | `drop_params: true` | Capability filter with `unsupported_params: drop`, and a report of what was dropped | – |
| 2.3 | `request_timeout`, `num_retries`, never retry a timeout | Section 4.4; template timeout up to 600 s | One retry layer: PACOD's or the gateway's |
| 2.4 | Chat completions, streaming with usage | `/v1/chat/completions` | – |
| 2.4 | `/v1/audio/speech` | `/v1/audio/speech`, priced per character | – |
| 2.4 | `/v1/models` as live registry | The workspace-scoped model list | – |
| 2.4 | `/utils/transform_request` | `playground/transform` | – |
| 3 | Message translation; Gemini history normalisation | Adapter normalises where the quirk profile requires it | `_normalize` may stay; it is harmless |
| 3 | Sampling parameters stripped per model family | Capability profiles | The capability table can be retired |
| 3 | Thinking through `extra_body.thinking`; `reasoning_effort` | Both accepted and mapped | – |
| 3 | Reasoning deltas normalised | `reasoning_content` deltas; other shapes per fixtures | The `<think>` wrapping for OpenWebUI |
| 3 | `cache_control` on the system prefix; cached-token reporting | Passed to Anthropic, removed elsewhere; `prompt_tokens_details.cached_tokens` | `build_cached_system` |
| 3 | `finish_reason` normalised | Section 8.4 | The retry on truncation |
| 3 | Anthropic web search tool; citations in `provider_specific_fields` | Passed through; same field | Citation parsing |
| 3 | Image input; text to speech | Image blocks translated; speech endpoint | – |
| 5.1 | Model catalogue, enable and disable, "pending restart" | Repository and life cycle; no restart | Reads the model list from the gateway |
| 5.2 | Templates with nine parameters | Workspace templates (a superset) | – |
| 5.3 | Role and template selection per profile | – | Stays: PACOD maps roles to `@template` |
| 5.4 | Capability table in code | Capability profiles as data | – |
| 5.5 | `RETIRED_MODELS` rewrite at boot | Successor resolved at request time | – |
| 5.6 | Agent editor overlay | – | Stays |
| 6 | Encrypted keys, `providers.env`, `make apply-llm-keys` | Credentials API, effective immediately | – |
| 7 | Timeout and retry rules | Section 4.4 | – |
| 8 | `UsageAccumulator`, `usage_events`, per-turn totals | Events with `trace_id` and tags; cost in every response; per-trace report | Per-turn totals in the SSE `done` event, summed from responses |
| 8.1 | Web search and speech booked as auxiliary cost | Speech natively; external costs through `POST /gw/v1/usage` (P2) | The web search itself |
| 8.3 | Cost formula; flat default; cached tokens at full price | Section 13.2 | – |
| 9 | `model_prices`, sources, daily job, manual rows protected, refresh API | Section 13 | – |
| 10 | Cheaper model per role, token caps, thinking, prompt-cache toggle | Templates | Semantic response cache, classifier cache, verification, memory |
| 11 | Quotas and throttle per user | Section 12 | The guard chain around them |
| 12 | `llm_*` series, four alert rules; workers not scraped | Section 14.7 and 14.9; workers included | HTTP and guard metrics |
| 13 | Usage and costs, analytics per site, profile and user, per turn | Section 14.5, with site and profile as tags | Web and page analytics |
| 14 | Debug Console, error log, call durations | Audit mode (section 14.6), the errors view and the gateway delay per call, for LLM calls | Non-LLM steps, turn evidence, audit of answers |
| 15 | Risks: unpinned image, no health check, no spend alert, no failover, shared key | Pinned releases, `/readyz`, spend limits and alerts, per-service keys; failover in P2 | – |

### 18.2 What must not change for PACOD at cutover

These are behaviours of the LiteLLM version PACOD runs, some of them undocumented. Each gets a fixture recorded from that version before the adapter work starts.

- Reasoning text in stream deltas, in the field or fields PACOD reads.
- `delta.provider_specific_fields.web_search_results` for Anthropic's web search.
- `usage.prompt_tokens_details.cached_tokens` on the final stream chunk, and whether `prompt_tokens` includes cached tokens.
- `finish_reason` values, in particular `length`.
- Acceptance of `thinking`, `reasoning_effort` and `cache_control` on any model, with unsupported ones dropped.
- The HTTP status and error shape PACOD's client classifies as a timeout, so that timeouts stay non-retried on PACOD's side.
- Model ids exactly as PACOD stores them.

### 18.3 Migration path

1. Record the fixtures of section 18.2 from the current proxy.
2. Run the gateway next to LiteLLM. Import PACOD's models, prices and templates through the management API; create the workspace and one key per service.
3. Shadow: replay recorded requests and compare response shape, usage and cost.
4. Switch the base URL and key one service at a time, workers first, the user-facing API last. Switching back is the same change in reverse.
5. Remove duplicates in PACOD step by step: capability stripping, the retry layer, the model catalogue and its restart flow, the price job, quotas, and finally usage accounting if PACOD no longer wants its own copy.

---

## 19. Catalogue mapping and phasing

### 19.1 Phases

| Phase | Theme |
|---|---|
| **P1** | PACOD parity and the requirement list: both modes, seven providers with Anthropic in full, organisations and workspaces, model repository, templates, native interface, management API, limits, usage and cost, price tracker, analytics, UI. |
| **P2** | Breadth and governance: native Gemini, embeddings and the Responses API, Claude on cloud platforms, fallback, credential pools, response cache, OIDC, MCP governance, first guardrails, reservations, Helm. |
| **P3** | Scale and advanced: capability broker and ranking, semantic cache, multi-region, sovereign pools, advanced guards, agent protocols. |

### 19.2 Build order inside P1

1. **Spine.** Configuration store, snapshot and hot reload, management API with authentication, organisations, workspaces and keys, providers and credentials, proxy mode with the usage extractor, event spool to ClickHouse with the gateway delay on every event, UI shell on the generated client.
2. **Managed mode, same dialect.** Model repository and life cycle, grants, templates in both modes, capability filter, cost, limits, the OpenAI-compatible adapter for six providers.
3. **Anthropic in full and the native interface.** The Anthropic adapter and the Anthropic-compatible interface (section 8.3), translation in both directions, the LiteLLM-compatible shapes, speech, `/gw/v1`, conformance fixtures. Batches and Files come last in this stage.
4. **Insight and cutover.** Price tracker, reports and UI pages, audit mode, dashboards and alerts; PACOD in shadow, then cutover.

Each stage ends with something that runs: after stage 1 the gateway is already a usable authenticated, accounted proxy.

### 19.3 Every catalogue feature, placed

A feature is listed under the phase where it first appears. Notes explain partial delivery.

| Domain | P1 | P2 | P3 |
|---|---|---|---|
| Interfaces | ING-01, ING-03, ING-04, ING-06, ING-07 (OpenAI ↔ Anthropic), ING-08, ING-09 (images, documents), ING-10 (speech), ING-12 (Anthropic), ING-16, ING-17 | ING-02, ING-05, ING-13 | ING-11, ING-14, ING-15 |
| Adapters | ADP-01, ADP-02, ADP-06, ADP-07, ADP-09, ADP-10, ADP-11 | ADP-03, ADP-08, ADP-12 | ADP-04, ADP-13, ADP-14 |
| Routing | RTE-01, RTE-03, RTE-12, RTE-13 | RTE-02, RTE-04, RTE-05, RTE-09 | RTE-06, RTE-07, RTE-08, RTE-10, RTE-11 |
| Tenancy | TEN-01 (two levels), TEN-02, TEN-06, TEN-09 | TEN-03 | TEN-04, TEN-05, TEN-07, TEN-08 |
| Policy | POL-03, POL-04, POL-06 (dry run, versions) | POL-02, POL-07, POL-08 | POL-01, POL-05, POL-09 |
| Quota | QTA-01, QTA-02, QTA-03, QTA-05, QTA-07, QTA-09 | QTA-04, QTA-06, QTA-08, QTA-10 | – |
| Guards | – | GRD-01, GRD-02, GRD-03, GRD-06, GRD-14, GRD-15 | GRD-04, GRD-05, GRD-07, GRD-08, GRD-09, GRD-10, GRD-11, GRD-12, GRD-13 |
| MCP | – | MCP-01, MCP-02, MCP-03, MCP-04, MCP-08, MCP-13, MCP-14 | MCP-05, MCP-06, MCP-07, MCP-09, MCP-10, MCP-11, MCP-12, MCP-15 |
| Broker | BRK-01, BRK-02, BRK-03, BRK-04 (passive), BRK-05, BRK-10, BRK-13 (model lists) | – | BRK-06, BRK-07, BRK-08, BRK-09, BRK-11, BRK-12 |
| Ledger | LED-01, LED-02, LED-03, LED-04, LED-05 (CSV), LED-06, LED-08 | – | LED-07 |
| Observability | OBS-01, OBS-02, OBS-03, OBS-05, OBS-06 | – | OBS-04 |
| Security | SEC-01, SEC-02, SEC-03, SEC-04, SEC-05, SEC-06, SEC-07, SEC-09, SEC-10, SEC-11 | – | SEC-08 |
| Operations | OPS-01, OPS-02, OPS-03, OPS-04, OPS-06, OPS-07, OPS-12 | OPS-05, OPS-10, OPS-11 | OPS-08, OPS-09 |
| Console | CON-01, CON-03, CON-04, CON-05 (prices, discovery), CON-06 | CON-02 | – |

ADP-05, the adapter-sourcing spike, is closed rather than phased: see the first item below.

### 19.4 Where this concept departs from the catalogue and the assessment

| Topic | Catalogue or assessment | This concept | Why |
|---|---|---|---|
| Language (ADP-05) | Go; embed `bifrost/core` or use an existing data plane; "Rust rewrite" dropped | Rust from the start; adapters written in-house | Your requirement. The cost is the adapter maintenance the assessment warns about, contained by two families, data-driven profiles, fixtures and proxy mode as a way around gaps. |
| Hot-path tuning | Called over-optimised | Explicit latency budgets, then stop | Your requirement for a fast, light core, bounded so it does not displace correctness. |
| Tenancy (TEN-01) | Five levels | Organisation → workspace → key | Your decision. Two rules replace a policy-composition engine. |
| Translation (ING-07) | Phase 2 | OpenAI ↔ Anthropic in P1 | PACOD reaches Anthropic through the OpenAI dialect, and Anthropic is supported in full. |
| Anthropic (ING-04, ING-12, ADP-02) | The Messages interface with pass-through of thinking in P1; batch and files optional in P3; Bedrock and Vertex with the adapter | The whole Messages API as interface and adapter in P1, with batches and files; Bedrock, Vertex AI and Foundry in P2 | Your decision. |
| Native interface (ING-08) | Phase 2, REST and gRPC | P1, REST only | Your requirement; it is the internal model anyway. |
| Speech (ING-10), end users (TEN-09), life cycle (BRK-03), price sheets (BRK-05) | Phase 2 or 3 | P1 | PACOD parity and your requirements. |
| MCP and guards | MCP governance and Tier 0 guards in P1 | Phase 2 | Your decision. PACOD uses neither. |
| Policy engine (POL-01) | CEL in P1 | Typed rules in P1; a policy language when MCP scopes need it | Nothing in P1 needs expressions. |
| Unsupported parameters | Fail loudly | Policy per workspace, default `drop`, always reported | PACOD depends on dropping. `reject` is available, and nothing is dropped silently. |
| Propagation (OPS-03) | Pushed snapshots over gRPC | Version polling against PostgreSQL | The same guarantee with fewer parts. Push can replace it behind the same version contract when instance counts make database connections a problem. |
| Counters (QTA-05) | Strict Valkey counters | In memory with one-second flushes; Valkey optional later | One fewer dependency; the overshoot bound is stated. |
| Ledger store (LED-01, LED-04) | Partitioned PostgreSQL, DuckDB for reports | ClickHouse | Already chosen in the scaffold; one analytical store. |
| Deployment modes (OPS-02) | `single` with SQLite, `cluster`, `scaled` | One mode on PostgreSQL | Two SQL dialects double the store work for little gain. |
| Fast path (ING-06) | Implicit when dialects match | An explicit proxy mode | Callers can see and choose what they get. |
| Routing (RTE-02, RTE-04 to RTE-08) | Load balancing and fallback P1 basic / P2; breakers, affinity, scores later | Template pools and organisation routing schemes with typed rules, optional CEL, eight algorithms, failover before the first byte, per-instance breakers | Your requirement. Rules are typed for speed and checkability; CEL covers the rest. Breakers and counters stay per instance, without a shared store. |
| Added | – | Workspace templates in both modes, audit mode per workspace and template, gateway delay per call, price tracker, Kimi, advisory limits, quota-state endpoint, LiteLLM-compatible shapes | Your requirements and PACOD parity. |

---

## 20. Risks and open decisions

### 20.1 Risks

| Risk | Why it matters | Mitigation |
|---|---|---|
| Provider drift | Seven providers change request and response details often. The assessment expects roughly one engineer of ongoing work. | Two adapter families; behaviour in quirk and capability data; fixtures in CI; proxy mode when an adapter lags. |
| Undocumented LiteLLM behaviour | PACOD parses shapes that exist only because LiteLLM produces them. | Fixtures recorded first; shadow run before cutover; switch one service at a time. |
| Size of the first release | Narrower in governance than the catalogue's first phase, but it adds a second complete dialect (Anthropic) with translation in both directions, life cycle, pricing and about twenty-five API resources with their pages. Full Anthropic support is the largest single addition. No team size or date has been set, and this concept claims neither. | The build order of section 19.2. If time is short, cut in this order: discovery, organisation-level credentials, managed Batches and Files, Anthropic-interface calls to other providers' models, the native speech endpoint. |
| Trust in the numbers | Missing usage on proxy streams and LLM-extracted prices can both be wrong. | `usage_source`, the `require` policy, bounded auto-apply with proposals, stored evidence, the accuracy report. |
| A single process holds every key | The core is the most valuable target. | Roles and separate listeners, envelope encryption, the threat model before code, no page fetching in the core. |
| Volume and sensitivity of audit records | A full record of every call grows fast: each turn of a conversation sends the history again, and attachments are large. The records hold prompts and answers. | Off by default; sampling, retention and an end time; attachments by reference where volume matters; storage ordered so that repetition compresses; access limited and logged; an organisation can forbid it. |
| Two stores | PostgreSQL and ClickHouse both need operating. | Analytics is optional; the minimal setup runs on PostgreSQL alone. |

### 20.2 Open decisions

| Decision | Options | Lean |
|---|---|---|
| Claude on Amazon Bedrock, Google Vertex AI and Microsoft Foundry | P2, or with the first release | P2, when a consumer needs one of them. |
| Cost in Anthropic-interface response bodies | Leave the body exactly as Anthropic returned it, or add a cost field | Unchanged body; cost in a header and in the request log. |
| Default behaviour of a template in proxy mode | `apply` (write the model and parameters) or `check` (write nothing, reject what does not conform) | `apply`. It changes parameter fields only, never the conversation content. Choose `check` where a body must stay unaltered to the byte. |
| Attachments in audit records | Stored in full, or by reference (type, size, hash) | In full, because the record should be complete. By reference where volume matters. |
| Does PACOD keep its own usage tables after cutover | Keep both, or rely on the gateway | Keep both through the shadow period, then decide. |
| Spool-full policy default | Keep serving, or fail closed | Keep serving; fail closed where usage is billed. |
| Trusted structured price feeds | To evaluate per provider | Start with pages plus extraction, which PACOD already proved; add feeds where a fixture shows they are right. |
| Reporting currency | One per installation, or per organisation | One per installation in P1. |
| Source of the key-encryption key in production | Environment or file, or KMS / Vault | File in P1; KMS in P2. |
| Workspace label on metrics | Always, or switchable | Switchable, for installations with very many workspaces. |
| Licence and name | – | Open, as in the catalogue. |

---

## Appendix A. Data model

### PostgreSQL

| Group | Tables |
|---|---|
| Tenancy | `organisations`, `workspaces`, `api_keys`, `tag_keys`, `settings` |
| Access | `users`, `role_bindings`, `admin_tokens` |
| Repository | `providers`, `credentials`, `models`, `capability_profiles`, `grants`, `discovery_proposals` |
| Provider objects | `provider_objects` (file and batch ids with their owning workspace) |
| Templates | `templates`, `template_versions` |
| Limits | `limits`, `limit_counters`, `end_user_overrides` |
| Pricing | `prices`, `price_sources`, `price_job_config`, `price_job_runs`, `price_proposals` |
| System | `config_versions`, `instances`, `job_leases`, `audit_log` |

**ClickHouse:** `usage_events`, `message_audit`, and the rollups of section 14.4.

## Appendix B. Example calls

### PACOD-style call with the OpenAI SDK, using a template

```python
client = OpenAI(base_url="http://pacod-gw-core:8080/v1", api_key=GATEWAY_KEY)

stream = client.chat.completions.create(
    model="@synthesizer",
    messages=messages,
    stream=True,
    stream_options={"include_usage": True},
    extra_body={"gateway": {
        "end_user": user_id,
        "trace_id": turn_id,
        "tags": {"purpose": "synthesizer", "site": site, "profile": profile_id},
    }},
)
```

### Proxy mode, Anthropic's own API with a gateway key

```sh
curl http://localhost:8080/proxy/anthropic/v1/messages \
  -H "x-api-key: $GATEWAY_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "content-type: application/json" \
  -d '{"model":"claude-sonnet-5-5","max_tokens":1024,
       "messages":[{"role":"user","content":"Hello"}]}'
```

### Proxy mode with a template

The body names a template instead of a model. The gateway writes the template's model and clamps the token limit; `messages` reaches Anthropic as the same bytes.

```sh
curl http://localhost:8080/proxy/anthropic/v1/messages \
  -H "x-api-key: $GATEWAY_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "content-type: application/json" \
  -d '{"model":"@planner","max_tokens":8000,
       "messages":[{"role":"user","content":"Plan the next step."}]}'
# With @planner = claude-haiku-4-5 and a ceiling of 400 output tokens, Anthropic receives:
# {"model":"claude-haiku-4-5","max_tokens":400,
#  "messages":[{"role":"user","content":"Plan the next step."}]}
```

### Managed mode, Anthropic's API with a template

The same request shape as above, sent to the gateway's root instead of `/proxy/anthropic`. The template supplies the model and its limits; everything else in the body reaches Anthropic unchanged.

```sh
curl http://localhost:8080/v1/messages \
  -H "x-api-key: $GATEWAY_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "content-type: application/json" \
  -H "x-gw-end-user: u_18342" -H "x-gw-trace: turn_9f3" \
  -d '{"model":"@synthesizer","max_tokens":4000,
       "thinking":{"type":"adaptive","display":"summarized"},
       "system":[{"type":"text","text":"You are a careful analyst.",
                  "cache_control":{"type":"ephemeral"}}],
       "messages":[{"role":"user","content":"Compare the two offers."}]}'
```

### Rotate a provider key without a restart

```sh
curl -X POST http://localhost:8081/api/v1/providers/prv_anthropic/credentials \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "content-type: application/json" \
  -d '{"label":"2026-10 rotation","secret":"sk-ant-...","make_current":true}'
# {"id":"cred_8H2","fingerprint":"...a91f","config_version":412,"applied":{"instances":3,"of":3}}
```

### Preview the effect of retiring a model

```sh
curl -X POST 'http://localhost:8081/api/v1/models/mdl_7Q2/transition?dry_run=true' \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "content-type: application/json" \
  -d '{"to":"retired","successor":"mdl_9KD"}'
# {"affected":{"organisations":1,"workspaces":2,"templates":["pacod-prod/@planner"],
#              "keys":0,"requests_last_30d":18422}}
```

## Appendix C. Changes to the current scaffold

Nothing in the scaffold was changed for this concept. Implementing it changes the following.

| File or area | Change |
|---|---|
| [architecture.md](architecture.md) | The UI no longer calls the analytics service; PostgreSQL is added; the diagram and component table follow section 3. |
| `crates/pacod-gw-events/src/lib.rs` | `tenant_id` becomes `organisation_id` and `workspace_id`; the fields of section 14.1 are added. |
| `crates/pacod-gw-analytics/migrations/0001_usage_events.sql` | New columns including the timing stages, duplicate removal by id, a sort key starting with organisation and workspace. A second migration adds `message_audit`. |
| `crates/pacod-gw-analytics/src` | A `jobs` module (price tracker, discovery, retention) next to `ingest`, `aggregate`, `reports`, `storage`. |
| `crates/pacod-gw-core/src` | Modules for the snapshot, the store, the canonical model, `proxy` and `managed` pipelines, adapters (`openai`, `anthropic`), limits, the spool and the management API. `providers/` and `routing/` are absorbed into these. |
| `config/gateway.example.toml` | The `[[providers]]` blocks go (providers live in the database). Added: role, database, secrets, spool. |
| `.env.example` | Provider keys go. Added: database URL, key-encryption key, HMAC pepper, first admin. |
| `ui/src/env.ts`, `ui/.env.example`, `ui/src/lib/api/client.ts` | One private core URL replaces the two public URLs; the hand-written client is replaced by the generated one. |
| `ui/src/lib/components/Nav.svelte`, `ui/src/routes` | Navigation and pages per section 15.2. |
| `deploy/docker-compose.yml` | PostgreSQL added; the analytics port is no longer published; image tags pinned instead of `latest`; health checks on `/readyz`. |

**Libraries proposed for the core** (to confirm when implementation starts): tokio and hyper with axum for the servers, a pooled hyper client with rustls, `arc-swap` for the snapshot, `sqlx` for PostgreSQL, `utoipa` for the OpenAPI document, the `metrics` facade with a Prometheus exporter, `opentelemetry-otlp` for traces, and `aes-gcm`, `hmac` and `sha2` for secrets and keys. The analytics service adds the `clickhouse` client.
