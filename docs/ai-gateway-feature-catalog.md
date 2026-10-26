# AI Broker & GW — Feature and functionality catalogue

| Field | Value |
|---|---|
| Date | 2026-10-03 |
| Status | Synthesis; input for design v0.2 |
| Inputs | `ai-gateway-design.md` v0.1 (not in this repository) · [design assessment](ai-gateway-design-assessment.md) · a market analysis (not in this repository) of Agent Router (ex Envoy AI Gateway), Bifrost, LiteLLM, Gravitee, OpenRouter and Kong |
| Method | Every feature in v0.1 was kept, changed or deferred according to the assessment; features the market analysis showed as missing or worth borrowing were added; each row is classed by how the market covers it and placed in a recommended phase. |

---

## 1. Summary

**158 features** in 14 domains: **44 differentiators** (no competitor covers them, or only a hosted one does), **70 table stakes** (every serious gateway ships them), **33 enablers** (internal architecture the others depend on) and **11 optional** items.

**Phasing (recommended, differs from v0.1 §20):** 77 rows touch **P1**, but most are small enablers or security constraints; the headline P1 scope is the twelve bullets in section 3. About 58 rows start in **P2** and 23 in **P3**. Even the re-cut P1 is only realistic in ~3 months for 3–4 engineers if the provider adapters are sourced from a library (ADP-05) rather than written in-house.

**What changed against v0.1 (the fifteen shifts that matter):**

1. The differentiation statement moves from "MCP brokering + content policy + tenancy + discovery" to the confirmed market gaps: reservation-based hard budgets, deep inherited tenancy, a self-hosted capability broker, sandboxed tools, a billing-grade ledger, and open sovereign packaging.
2. MCP governance (virtual server, tool scopes, tool pinning) moves **into P1**; cross-dialect translation, discovery ranking, lease quotas, semantic cache and own SDKs move **out of P1**.
3. Provider adapters are sourced by a spike (embed `bifrost/core` or use Agent Router as data plane); own adapters only where the library falls short.
4. Virtual keys are verified by HMAC-SHA256 lookup, not argon2id.
5. Configuration reaches edges as versioned snapshots pushed over gRPC with ACK, not PostgreSQL `LISTEN/NOTIFY`.
6. The edge stays pure Go; Tier 1 ML classifiers run in a separately scaled guard service by default, and DuckDB moves to a `report`/control role.
7. Quotas start with strict atomic Valkey counters; lease-based slices become a P3 optimisation.
8. The usage ledger gets a durable local spool before acknowledgement.
9. MCP: a credential vault with on-behalf-of token exchange replaces "OAuth passthrough"; sessions are stateless (state encrypted into the session id); tool definitions are hash-pinned; stdio servers are admin-approved and sandboxed only.
10. Streaming guard semantics are defined: detect-only passes through, redact buffers segments, block holds back or buffers fully; tool calls and structured output always buffer.
11. Prompt-injection defence gains architectural controls (session taint tracking, tool trust levels, human approval); classifiers become signals and audit, not the control.
12. A threat model and eight hardening items drawn from the 2026 incidents at LiteLLM, Bifrost and Agent Router are added to P1.
13. "Lossless translation" is replaced by a per-adapter feature matrix with fail-loud behaviour; the Responses API is stateless-only and leaves P1.
14. The resource model is per endpoint (model × deployment), and discovery becomes a three-stage pipeline (hard filters → soft percentile thresholds → sort or price-weighted pick), borrowed from OpenRouter.
15. Dropped: the "Rust rewrite of the edge" open decision, cost normalisation across resource kinds, and traffic-dependent computed local cost.

---

## 2. How to read the tables

| Column | Values |
|---|---|
| **Class** | **DF** differentiator (confirmed gap across the six products) · **TS** table stakes · **EN** enabler · **OPT** optional, demand-driven |
| **Phase** | **P1** vertical slice (core proxy + MCP governance) · **P2** platform (broker, Tier 1 guards, chargeback) · **P3** scale and advanced (observed-metric ranking, multi-region, A2A, operator) |
| **Origin** | `=` kept from v0.1 · `~` changed · `+` new · `↓` deferred later than v0.1 planned; **D§** design v0.1 section · **A§** assessment section · **M:** market analysis, product the idea comes from |

---

## 3. The P1 vertical slice (what ships together)

1. OpenAI Chat/Embeddings and Anthropic Messages northbound, **same-dialect passthrough only** (ING-01…04, ING-06).
2. OpenAI-compatible and Anthropic adapters sourced per ADP-05, with conformance fixtures (ADP-01, 02, 06).
3. Virtual keys with HMAC lookup, OIDC console login, basic RBAC (TEN-02, 03, 06).
4. Tenancy schema and inheritance engine for all five levels; tooling exposes tenant, project and key (TEN-01).
5. CEL `route_filter`, `constraint` and `tool_scope` policies with defined composition semantics and residency hard filter (POL-01…05, 07).
6. Spend and token budgets, reserve-then-settle on strict Valkey counters (QTA-01…05, 09).
7. Tier 0 guards on input, output, tool arguments and results, with the three streaming modes (GRD-01…03, 06, 14, 15).
8. MCP virtual server with deny-by-default tool scopes, tool-definition pinning and stateless sessions (ING-13, MCP-01…04, 08, 13, 14).
9. Static aliases, weighted fallback, retries, policy-preserving fallback (RTE-01…03, 05, 12).
10. Durable usage ledger, audit events, cost in every response (LED-01…03, 06).
11. Prometheus, OTel GenAI traces (content off), structured logs (OBS-01…03).
12. Single static binary, `single` and `cluster` modes, pushed snapshots, Helm/air-gapped install, minimal console and CLI (OPS-01…05, 07, 11, 12; CON-01).

Plus the P1 security constraints (SEC-01…07, 09…11), which are design rules rather than work packages.

---

## 4. Northbound interfaces (ING)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| ING-01 | OpenAI Chat Completions | `/v1/chat/completions` with SSE streaming, tool calling, structured outputs, image inputs | TS | P1 | = D§6.1 |
| ING-02 | OpenAI Embeddings | `/v1/embeddings`, batched inputs, dimension parameter passthrough | TS | P1 | = D§6.1 |
| ING-03 | Model listing | `/v1/models` returns only the aliases the caller's scope permits | TS | P1 | = D§6.1 |
| ING-04 | Anthropic Messages | `/v1/messages` with streaming, tools, extended-thinking passthrough; `/v1/messages/count_tokens` | TS | P1 | = D§6.1 · + M: Agent Router, Gravitee (count_tokens) |
| ING-05 | OpenAI Responses API | `/v1/responses`; v1 is stateless only and rejects `store` / `previous_response_id` with 400; a stateful mode needs a conversation store (later). Built: `store` is false at the provider; a workspace can have the gateway keep responses (`responses.store`, with retention and erasure) and replay them for `previous_response_id`; `conversation`, `background`, stored prompts and item references are refused | TS | P2 | ~ A§4.4 · + M: OpenRouter (stateless pattern) |
| ING-06 | Same-dialect fast path | Byte-level passthrough when caller dialect = provider dialect and no transform applies; only `usage` and tool-call deltas parsed | EN | P1 | = D§8.2 |
| ING-07 | Cross-dialect translation | Anthropic ↔ OpenAI-compatible, OpenAI → Gemini / Bedrock; opt-in per alias; per-adapter feature matrix; unsupported fields return `400 unsupported_feature`, never dropped silently | TS | P2 | ~ A§4.4 ("lossless" dropped) |
| ING-08 | Native gateway API | `POST /gw/v1/invoke` (canonical request), `/gw/v1/batch`, discovery and registry endpoints; REST + gRPC via connect-go | EN | P2 | = D§6.1, App. D |
| ING-09 | Multimodal content parts | Image, audio and document parts in the canonical schema; MIME validation, size caps, blob staging with TTL; binaries never logged | TS | P2 | = D§5 |
| ING-10 | Audio, image and rerank endpoints | Transcription/TTS, image generation and rerank routed to the matching resource kinds. Built: speech; image generation and edits; transcription and translation; rerank | TS | P3 | ↓ D§1 (kinds listed, endpoints not) · + M: Bifrost, LiteLLM, Kong |
| ING-11 | Realtime (WebSocket) proxy | OpenAI Realtime / Gemini Live with policy and budget check on connect. Built: proxy mode relays WebSockets; managed OpenAI Realtime at `/v1/realtime`, priced per response | OPT | P3 | + M: Bifrost, LiteLLM, Kong (only if demanded) |
| ING-12 | Batch and Files | Provider batch APIs (≈50 % discount) and file passthrough with tenant scoping. Built: Anthropic's and OpenAI's | OPT | P3 | + A§5 |
| ING-13 | MCP virtual server | `/mcp` Streamable HTTP server exposing the caller's permitted tools plus `discover_resources` / `describe_resource` | DF | P1 | ~ D§6.1 (moved from Phase 2) |
| ING-14 | A2A endpoint | Agent-to-agent JSON-RPC proxy with agent-card rewriting | OPT | P3 | = D§6.1 · + M: Kong, LiteLLM |
| ING-15 | Client SDKs | TypeScript/Python wrappers; rely on OpenAI/Anthropic SDK compatibility first | OPT | P3 | ↓ A§3.3 |
| ING-16 | Request metadata and tags | `end_user_id`, free-form tags and `session_id` accepted via headers or body; used for attribution, tag budgets and sticky routing | TS | P1 | + A§5 · M: OpenRouter, LiteLLM |
| ING-17 | Ingress basics | Dialect detection, request id, OTel span, body size limits, idempotency-key passthrough | EN | P1 | = D§8 |

## 5. Southbound adapters (ADP)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| ADP-01 | OpenAI-compatible adapter | OpenAI, Azure OpenAI, Mistral, Groq, vLLM, Ollama, TGI, llama.cpp, LM Studio; EU providers (Scaleway, OVHcloud, Nebius, IONOS/STACKIT via custom endpoint); Gemini via its OpenAI-compatible endpoint | TS | P1 | = D§6.2 · + M: LiteLLM (EU list) |
| ADP-02 | Anthropic adapter | Anthropic API; Claude on Bedrock and Vertex | TS | P1 | = D§6.2 |
| ADP-03 | Google adapter | Native Gemini API and Vertex AI (thought signatures kept same-vendor) | TS | P2 | ↓ D§6.2 (Gemini reachable through ADP-01 in P1) |
| ADP-04 | Bedrock adapter | Converse / InvokeModel for non-Anthropic Bedrock models; SigV4 / IRSA auth | TS | P2 | = D§6.2 |
| ADP-05 | Adapter sourcing decision | Two-week spike: embed `bifrost/core` (Apache-2.0 Go) behind an internal adapter interface, or run Agent Router as the data plane; write own adapters only where the library falls short | EN | P1 (decision) | + M§6 Options A/B |
| ADP-06 | Conformance suites | Recorded fixtures per adapter: streaming, tool calls, usage, reasoning tokens, cache control, error mapping; run in CI | EN | P1 | = D§6.2 |
| ADP-07 | Feature matrix per adapter | Machine-readable list of supported parameters per provider, surfaced by `describe_resource`; drives fail-loud behaviour | EN | P2 | + A§4.4 |
| ADP-08 | Upstream credential pools | Several keys per provider; 401/403 marks a key dead, 429 rotates with backoff; per-key TPM/RPM awareness and spillover | TS | P2 | + A§5 · M: Bifrost |
| ADP-09 | Provider prompt-cache support | Unified `cache_control` passthrough, optional automatic breakpoint insertion, cached-token accounting | TS | P2 | + M: Agent Router, Bifrost |
| ADP-10 | Reasoning handling | Thinking blocks and signatures preserved only within the same vendor; one `reasoning_effort` mapped across vendors; reasoning tokens in `usage` | EN | P1 / P2 | ~ A§4.4 · + M: Agent Router |
| ADP-11 | Response normalisation | Normalised `finish_reason` plus `native_finish_reason`; `usage` with prompt/completion/cached/reasoning tokens and `cost` on every response | EN | P1 | + M: OpenRouter |
| ADP-12 | MCP client | Streamable HTTP and legacy HTTP+SSE upstreams (P1); stdio only through the sandbox bridge (P2) | DF | P1 / P2 | = D§6.2 |
| ADP-13 | OpenAPI tool wrapper | Any REST API described by OpenAPI becomes a governed tool | TS | P3 | = D§6.2 · + M: Kong, Gravitee |
| ADP-14 | OpenRouter as optional upstream | `eu.openrouter.ai` registered as a provider for long-tail, non-sensitive models with `zdr`, `data_collection: deny` and pinned provider order; `usage.cost` ingested | OPT | P3 | + M: OpenRouter |

## 6. Routing, reliability and caching (RTE)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| RTE-01 | Model aliases | Alias = pinned list (P1) or stored discovery query (P2); scoped per tenant/project; version pin or float | TS | P1 | = D§5, §7.7 |
| RTE-02 | Load balancing and fallback | Weighted spread across replicas and providers; priority fallback chains; `failover_criteria` (429, 5xx, timeout, context length, content policy, non-idempotent) | TS | P1 basic / P2 criteria | = D§5 · + M: Kong, LiteLLM |
| RTE-03 | Retries and timeouts | Retries with jitter per error class; per-model timeouts; stream-idle timeout with failover before the first byte, 504 after | TS | P1 | = D§5 · + M: Agent Router |
| RTE-04 | Circuit breakers | Per resource, half-open probes, fallback chain from discovery | TS | P2 | = D§13 |
| RTE-05 | Policy-preserving fallback | Budgets, residency and guard profile re-evaluated for every fallback target | EN | P1 | + M: LiteLLM |
| RTE-06 | Session and prefix affinity | `session_id` sticky routing and prefix-hash affinity to protect provider and vLLM prompt caches | TS | P2 | + A§4.10 · M: OpenRouter |
| RTE-07 | Score-based routing | Cost, latency and quality ranking supplied by the capability broker (BRK-06) | DF | P2 / P3 | = D§7.6 |
| RTE-08 | Canary and shadow traffic | Weighted canary for new resource versions; shadow (duplicate and discard) for evaluation | TS | P2 | = D§5 |
| RTE-09 | Exact response cache | Key = tenant + resource + system-prompt hash + normalised params; Valkey; streamed replay; never cross-tenant | TS | P2 | = D§5 |
| RTE-10 | Semantic cache | Opt-in, per-tenant threshold, Qdrant or pgvector | OPT | P3 | ↓ D§5 |
| RTE-11 | Local-GPU pool awareness | vLLM metrics (queue depth, KV-cache utilisation, LoRA adapters) feed capacity-aware spreading; optional Gateway API Inference Extension integration | DF | P2 | + M: Agent Router |
| RTE-12 | Backpressure | Per-pod in-flight stream cap, priority classes, `429 Retry-After`, soft queue | EN | P1 cap / P2 priority | = D§13 |
| RTE-13 | Long-running requests | 10+ minute streams, async completion with webhook, graceful pod drain | EN | P2 | + A§5 |

## 7. Identity, tenancy and access (TEN)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| TEN-01 | Tenancy hierarchy | Tenant → Team → Project → Principal → VirtualKey; policies, budgets, guard profiles and tool scopes attach at any node and inherit downward; children can only restrict | DF | P1 schema + engine / P2 team & principal tooling | = D§11 · ~ A§3.3 |
| TEN-02 | Virtual keys | High-entropy random keys, prefix-identifiable, **HMAC-SHA256 lookup in the snapshot**, scopes, expiry, rate class, instant revocation, rotation with grace window | TS | P1 | ~ A§4.1 · + M: Bifrost, OpenRouter |
| TEN-03 | Human identity (OIDC) | Any OIDC IdP for console and playground; group → role mapping | TS | P1 / P2 mapping | = D§5 |
| TEN-04 | Workload identity | mTLS/SPIFFE for services; RFC 8693 token exchange from Kubernetes service-account or IdP tokens into short-lived gateway tokens | TS | P2 | = D§5 · + M: OpenRouter, LiteLLM |
| TEN-05 | Delegated agent identity | Agents act on behalf of users through delegated tokens, never shared keys; delegation chain recorded in the ledger | DF | P2 | + A§4.11 · M: Gravitee |
| TEN-06 | Admin RBAC | Platform admin, tenant admin, team admin, project admin, viewer, auditor; custom roles later | TS | P1 / P3 custom | = D§5 |
| TEN-07 | SCIM provisioning | Users and groups synchronised from the IdP | OPT | P3 | + M: Bifrost, LiteLLM, OpenRouter |
| TEN-08 | Sovereign tenant cells | `sovereign` flag → dedicated edge pool holding only that tenant's provider keys and data | DF | P3 | ~ D§11 · A§4.11 |
| TEN-09 | End-user attribution | `end_user_id` tracked per request; optional per-end-user budgets and rate limits | TS | P2 | + M: LiteLLM |

## 8. Policy engine (POL)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| POL-01 | CEL policy engine | Compiled once per snapshot; static cost limits on tenant-authored expressions; evaluation errors deny | EN | P1 | = D5 · + A§4.6 |
| POL-02 | Policy types | `route_filter`, `constraint`, `tool_scope` (P1); `guard`, `discovery_scope` (P2) | EN | P1 / P2 | = D§11 |
| POL-03 | Composition semantics | Deny-overrides; per-attribute lattice for constraints (minimum of caps, intersection of allowlists); documented precedence by node depth and priority | EN | P1 | + A§4.6 · M: OpenRouter |
| POL-04 | Typed constraints | Constraints (max tokens, allowed tools, priority) as typed config; CEL only for predicates | EN | P1 | ~ A§4.6 |
| POL-05 | Policy inputs | `request`, `principal`, `tenant`, `project`, `resource`, `tool`, `content`, `verdict`, `budget`, plus `session.taint` | EN | P1 / P2 taint | = D App. C · + GRD-08 |
| POL-06 | Lint, dry-run, versioning | Policy linting, shadow evaluation against recorded traffic, versioned policies with audit | TS | P2 | = D§18 |
| POL-07 | Residency and classification filters | Tenant-level hard filters on `resource.residency` and `data_classification_max`; fail closed (no out-of-region fallback) | DF | P1 residency / P2 classification | = D§5 · + M: OpenRouter |
| POL-08 | System-prompt pinning | Per-route system-prompt injection or pinning | TS | P2 | = D§9 |
| POL-09 | Policy presets | Named shorthands (`cheapest`, `fastest`, `eu-only`) attachable to aliases or keys | OPT | P3 | + M: OpenRouter |

## 9. Quota, budgets and cost control (QTA)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| QTA-01 | Budget types | Tokens by type (prompt, completion, cached, reasoning), requests, spend in currency, per model/tool/kind; CEL cost expressions to weight token types | DF | P1 / P2 CEL weights | = D§10 · + M: Agent Router |
| QTA-02 | Budget scopes | Any hierarchy node, tag or end-user; inherited; most specific wins; hard caps cannot be loosened | DF | P1 | = D§10 · + M: LiteLLM |
| QTA-03 | Windows | Fixed (day/month, calendar-aligned), sliding (RPM/TPM), lifetime | TS | P1 | = D§10 |
| QTA-04 | Reserve-then-settle | Estimated tokens/spend reserved before the upstream call (in-flight hold), settled on actual usage; `402` / `429` with `Retry-After` and remaining-budget headers | DF | P1 | = D§8 · + M: OpenRouter |
| QTA-05 | Distributed accuracy | P1: strict atomic Valkey counters (Lua); P3: lease-based local slices with bounded overshoot for multi-region or very high RPS | DF | P1 / P3 | ~ A§3.4 (D9 re-ordered) |
| QTA-06 | Mid-stream cutoff | Defined behaviour when a hard budget is exhausted during generation: terminate with an error event, bill actual usage, no retry | DF | P2 | + A§4.8 |
| QTA-07 | Soft limits and alerts | 50/80/100 % alerts, temporary budget increases, soft queue by priority class | TS | P2 | = D§10, §15 · + M: LiteLLM |
| QTA-08 | Reconciliation | Ledger is the source of truth; hourly counter reconciliation; monthly reconciliation against provider usage and invoices | EN | P2 | = D§10 · + A§4.9 |
| QTA-09 | Explicit edge semantics | `max_budget: 0` means zero allowance; budgets apply to each fallback target; defined reservation strategy for reasoning tokens | EN | P1 | + M: LiteLLM · A§4.8 |
| QTA-10 | Contract prices | Tenant × resource negotiated price overrides applied at settlement; list price fallback | DF | P2 | = D§7.5 |

## 10. Content control and guardrails (GRD)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| GRD-01 | Tier 0 inline checks | RE2 regex, DLP and secret patterns, JSON Schema, size and length limits, language detection; microsecond cost | TS | P1 | = D§9 |
| GRD-02 | Guard hook points | `pre_call` (input), `during_call` (stream window), `post_call` (output), `pre_tool` (arguments), `post_tool` (results) | EN | P1 | = D§8 · + M: LiteLLM (naming) |
| GRD-03 | Streaming guard modes | Detect-only passes through; redact buffers segments; block holds back N tokens or buffers the whole stream; tool calls and structured output always fully buffered; mid-stream block → error event, actual usage billed, no retry | DF | P1 | + A§4.7 · M: Bifrost, Kong |
| GRD-04 | Tier 1 ML classifiers | Prompt-injection, PII NER and toxicity classifiers in a separately scaled guard service (gRPC, latency budget, CPU or GPU); in-process only in `single` mode | TS | P2 | ~ A§4.2 (D11 changed) · M: Gravitee (footprint evidence) |
| GRD-05 | Tier 2 LLM judge | Local judge model; verdicts allow / refuse / rewrite / ask-human; shadow by default | TS | P3 | = D§9 · + M: Gravitee |
| GRD-06 | Fail-safe defaults | Fail closed on guard error or timeout; fail-open only as an explicit per-route opt-in | EN | P1 | = D§3 · + M (Bifrost and Gravitee judges fail open) |
| GRD-07 | Reversible redaction | PII replaced by placeholders and restored in the response when policy allows | TS | P2 | + M: Kong, Bifrost |
| GRD-08 | Session taint tracking | Once untrusted content (tool result, fetched page) enters a session, sensitive tools (send, write, external) require approval or are blocked | DF | P2 | + A§4.12 |
| GRD-09 | Tool trust levels | Tools classed trusted / untrusted / blocked, with trust-chain checks when one tool's results feed another | DF | P2 | + M: LiteLLM |
| GRD-10 | Human-in-the-loop approvals | Approval queue for sensitive tool calls using MCP elicitation; timeout → deny | DF | P3 | + A§4.12 · M: Gravitee |
| GRD-11 | Tool-definition scanning | Tool names and descriptions scanned for injection at registration and on change; hash pinned (MCP-03) | DF | P2 | + A§4.5 |
| GRD-12 | Content constraints | Topic allow/deny, language constraints, output schema enforcement, watermark and metadata headers | TS | P2 | = D§9 |
| GRD-13 | External guard connectors | Generic HTTP/gRPC guard adapter for Presidio, Bedrock Guardrails, Azure Content Safety, Lakera, Model Armor | TS | P3 | + M: LiteLLM, Kong, Bifrost |
| GRD-14 | Verdict logging | Rule id, action and confidence per verdict into the ledger; guard analytics | TS | P1 | = D§9 |
| GRD-15 | Guard profiles | Ordered rule sets with tier, mode (inline or shadow) and action per node; sampling for expensive tiers | TS | P1 | = D§11 · + M: Bifrost (sampling) |

## 11. MCP and tools (MCP)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| MCP-01 | Virtual MCP server | Aggregates permitted upstream tools under `server.tool` namespaces; per-project tool bundles limit tool-count explosion | DF | P1 | = D§5 · + M: Bifrost, Gravitee |
| MCP-02 | Tool scopes (ACL) | Deny-by-default over tools, resources and prompts; allow/deny per node; CEL argument constraints; `tools/list` filtered by the same rules | DF | P1 | = D§11 · + M: Gravitee, Agent Router |
| MCP-03 | Tool-definition pinning | Hash of each tool definition recorded at approval; a change disables the tool until re-approved | DF | P1 | + A§4.5 |
| MCP-04 | Stateless sessions | Session state encrypted into `Mcp-Session-Id`, so any edge serves any session without a store | EN | P1 | + M: Agent Router |
| MCP-05 | Server-initiated features | Per-route policy for notifications (`tools/list_changed`), sampling, elicitation and roots | EN | P2 | + A§4.5 |
| MCP-06 | Upstream credential vault | Per-tenant and per-user upstream credentials; OAuth 2.0/2.1 consent flows; on-behalf-of token exchange; **no raw token passthrough** | DF | P2 | ~ A§4.5 (replaces "OAuth passthrough") · M: Bifrost |
| MCP-07 | Gateway as OAuth resource server | RFC 9728 protected-resource metadata, audience binding and PKCE for MCP consumers | TS | P2 | + M: Bifrost, Kong, Agent Router |
| MCP-08 | Tool-call modes | Pass-through (client executes, gateway checks call and result) in P1; brokered loop (gateway executes, guards result, returns to model) in P2; default per route | DF | P1 / P2 | = D§8.1 |
| MCP-09 | Sandboxed stdio servers | stdio servers only in separate gVisor/Kata pods behind a stdio → HTTP bridge; registered by admins with approval, never through a runtime API; lifecycle and scale-to-zero | DF | P2 | = D§14.2 · ~ M (Bifrost CVE-2026-90898) |
| MCP-10 | Introspection onboarding | Probe `initialize` + `tools/list`, draft a manifest, admin approval | TS | P2 | = D§7.2 |
| MCP-11 | OpenAPI → MCP conversion | Governed REST APIs exposed as MCP tools | TS | P3 | + M: Kong, Gravitee |
| MCP-12 | Tool-call validity tracking | Model-emitted tool calls validated against the schema (InvalidJson / UnknownName / SchemaMismatch) per resource, as a quality signal | DF | P2 | + M: OpenRouter |
| MCP-13 | Result limits and filtering | Size caps, content-type checks and field filtering on tool results | TS | P1 | = D§5 |
| MCP-14 | Tool audit | Every invocation logged: principal, tool, argument hash, verdict, latency | TS | P1 | = D§16 |
| MCP-15 | A2A proxy | Agent cards, per-key permissions, iteration budgets, kill switch | OPT | P3 | = D§6.1 · + M: LiteLLM |

## 12. Capability broker and registry (BRK)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| BRK-01 | Resource model | One `Resource` with `kind`; **modelled per endpoint** (model × deployment) carrying price, quantisation, context, features, data policy, region, service tier and declared quality | DF | P1 schema / P2 full | = D§7.1 · ~ M: OpenRouter |
| BRK-02 | Registration paths | Config-as-code YAML and admin API/console (P1); signed self-registration with heartbeat TTL and introspection (P2); Kubernetes operator CRD (P3) | DF | P1 → P3 | = D§7.2 |
| BRK-03 | Lifecycle | `registered → validating → pending_approval → active → degraded → draining → retired`; versions coexist | DF | P2 | = D§7.3 |
| BRK-04 | Health and metrics | Leader-elected active probes, budgeted synthetic canaries, passive EWMA per edge (TTFT, tokens/s, p95, error class, 429 rate) summarised to Valkey; hysteresis state machine | DF | P2 | = D§7.4 · + A§5 (canary cost) |
| BRK-05 | Cost model | Price sheets per version with modality-aware fields (cache read/write, context factors, service tier, per call/second/image/tool); computed local rate = GPU cost ÷ **capacity-normalised** throughput; contract overrides; never retroactive | DF | P2 | = D§7.5 · ~ A§4.10 · + M: Kong |
| BRK-06 | Discovery and ranking pipeline | Hard filters (scope, capabilities, residency, classification, health, capacity, max price, quantisation, data policy) → soft percentile thresholds that deprioritise → explicit sort or outage-aware inverse-square-price weighted pick → ordered fallback; rank within kind only; deterministic tie-break; score breakdown returned | DF | P2 filters + sort / P3 weighted pick + observed metrics | ~ D§7.6 · M: OpenRouter |
| BRK-07 | Quality signals | Tool-call validity rate, canary benchmarks, declared eval scores with provenance; observed values after N samples; recomputed about every 5 minutes | DF | P3 | = D§3.6 · + M: OpenRouter |
| BRK-08 | Signed bindings | Opaque, TTL-bound tokens referencing a resolved resource; callers never see endpoints or credentials | DF | P2 | = D§7.6 |
| BRK-09 | Discovery exposure | Native REST/gRPC and MCP tools `discover_resources` / `describe_resource`; the router uses the same engine | DF | P2 | = D§7.6 |
| BRK-10 | Visibility and scope | `global / tenant / project` visibility; `discovery_scope` policies | DF | P2 | = D§7.1 |
| BRK-11 | Capacity tracking | Concurrency in use vs declared max; vLLM queue-depth scrape | DF | P2 | = D§7.4 |
| BRK-12 | Catalogue and approval workflow | Register → approve → publish → discover; owner, version, risk, auth and policy metadata; semantic search | TS | P3 | + M: Kong, Gravitee |
| BRK-13 | Import mappings | A2A Agent Card, MCP `initialize` / `tools/list`, OpenAPI and `/v1/models` normalised into the descriptor | DF | P2 / P3 | = D§7.1 |

## 13. Usage ledger, reporting and chargeback (LED)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| LED-01 | Durable usage events | Per-request event (hierarchy ids, resource, tokens by type, list and effective cost, latency, cache, verdicts, status, tags, end-user); **local append-only spool before acknowledgement**, batched `COPY` into partitioned PostgreSQL | DF | P1 | ~ A§4.9 |
| LED-02 | Partitioning, retention, erasure | Monthly partitions; per-tenant retention; GDPR erasure procedure covering partitions and Parquet exports | DF | P1 / P2 erasure | = D§15 · + A§4.11 |
| LED-03 | Audit events | Every control-plane mutation and policy denial with actor, target and before/after hash; tamper-evident chain | TS | P1 / P2 chain | = D§11, §16 |
| LED-04 | Showback and chargeback | Reports per hierarchy node, model and tool mix, guard activity, SLOs, anomaly flags; DuckDB over Parquet in the `report`/control role only | DF | P2 | = D§15 · ~ A§4.2 |
| LED-05 | Exports | CSV/Parquet, invoice-style statements, object-storage export; optional log-export connectors (Kafka, S3, BigQuery) | TS | P2 / P3 | = D§15 · + M: Bifrost |
| LED-06 | Cost visibility per request | `usage.cost` and upstream cost in every response; usage API per key, project and tenant | TS | P1 | + M: OpenRouter |
| LED-07 | Provider reconciliation | Ledger compared with provider usage and invoices; drift alerts | EN | P2 | + A§4.9 |
| LED-08 | Alerting | Budget thresholds, error rate per resource, guard-block spikes, health transitions via webhook, email or chat | TS | P2 | = D§15 |

## 14. Observability (OBS)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| OBS-01 | Prometheus metrics | Requests, tokens, cost, upstream latency, TTFT, guard verdicts, quota denials, in-flight streams, cache hits and resource health with tenant/route/resource labels | TS | P1 | = D§15 |
| OBS-02 | OpenTelemetry traces | GenAI semantic conventions; one span per stage, upstream span with provider request id, tool spans; **content capture off by default**, opt-in with redaction | TS | P1 | = D§15 · + M: Agent Router (default-on pitfall) |
| OBS-03 | Structured logs and replay | `slog`; bodies off by default; redacted per-tenant sampling; replay from the stored canonical request (opt-in, retention-bound) | TS | P1 | = D§15 |
| OBS-04 | Export destinations | OTLP collector plus LLM-observability targets (Langfuse/LangSmith-style, Datadog); per-destination privacy mode; session-coherent sampling | OPT | P3 | + M: OpenRouter, LiteLLM |
| OBS-05 | Latency breakdown | Per-stage timing exposed in logs and console | TS | P2 | + M: Bifrost |
| OBS-06 | Dashboards and alert rules | Shipped Grafana dashboards and Prometheus alert rules | TS | P2 | + M: Agent Router |

## 15. Security (SEC)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| SEC-01 | Threat model | Written before code: provider-key blast radius, SSRF, tool poisoning, cross-tenant leakage, admin and insider, gateway bypass, supply chain | EN | P1 | + A§4.11 |
| SEC-02 | Provider-key protection | Envelope encryption with KMS, Vault or OpenBao; decrypted only into edge memory; rotation via control plane; never logged | TS | P1 | ~ D§16 · A§4.11 |
| SEC-03 | Hardened admin surface | Admin API and console only in the `control` role on a separate listener; authenticated by default; no dynamic plugin or code loading in the edge | TS | P1 | + M: Bifrost CVEs |
| SEC-04 | Strict JSON decoding | Case-sensitive field matching, duplicate-key rejection and unknown-field handling on every policy-relevant path; fuzz tests | EN | P1 | + M: Agent Router GHSA-4gph-2hhr-5mwg |
| SEC-05 | SSRF protection | Egress allowlists for introspection, the OpenAPI wrapper and MCP upstreams; link-local and cluster CIDRs blocked; HTTP CONNECT egress proxy support | TS | P1 | + A§4.11 · M: Bifrost CVE, Agent Router |
| SEC-06 | Bypass prevention | NetworkPolicy templates so model servers accept traffic only from edge pods; no `auth: none` resource reachable from workloads | TS | P1 | + A§4.11 |
| SEC-07 | Transport security | TLS everywhere, mTLS for service principals, HTTP/2 upstream pools | TS | P1 | = D§16 |
| SEC-08 | Residency beyond routing | Traces, logs, caches, replay store and ledger kept regional; regional Valkey and edges; fail closed | DF | P3 | = D§16 · + A§4.11 |
| SEC-09 | Supply chain | Pinned modules, SBOM, cosign-signed images, distroless base, reproducible builds, dependency scanning | TS | P1 | = D§16 · + M: LiteLLM incident |
| SEC-10 | Secret hygiene | Tier 0 secret detection in content; Authorization headers never logged; redaction before any export | TS | P1 | = D§9 · + M: LiteLLM incident |
| SEC-11 | Disclosure process | Security policy, advisories, embargo handling | EN | P1 | + M (all vendors) |

## 16. Deployment, operations and architecture (OPS)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| OPS-01 | Single static binary | Pure-Go `edge`; cgo-dependent parts (ONNX, DuckDB) confined to `control` / `report` roles or separate services; distroless image; roles `edge`, `control`, `report`, `all` | EN | P1 | ~ A§4.2 (D3, D11, D12 adjusted) |
| OPS-02 | Deployment modes | `single` (SQLite, in-memory), `cluster` (PostgreSQL + Valkey), `scaled` (+ NATS, guard service, object storage, Qdrant) | EN | P1 / P3 scaled | = D§14.1 |
| OPS-03 | Snapshot propagation | Control plane builds versioned snapshots and pushes them to edges over a gRPC stream with ACK and full resync on reconnect; separate fast channel for revocations, health and budgets; no `LISTEN/NOTIFY` dependency | EN | P1 | ~ A§4.3 |
| OPS-04 | Immutable snapshot | Config, policies, keys, routes, registry, prices and health as one immutable struct; atomic swap; compiled CEL; zero DB on the request path | EN | P1 | = D§13 |
| OPS-05 | Kubernetes packaging | Helm, HPA on in-flight streams and CPU, PDB, NetworkPolicy, Gateway API ingress; RKE2 validated; **air-gapped install** with bundled price sheets and no outbound fetches | TS | P1 | = D§14.2 · + M: Bifrost, LiteLLM (startup fetches) |
| OPS-06 | Leader election | PostgreSQL advisory lock for probes, canaries and scheduled jobs | EN | P2 | = D§14.2 |
| OPS-07 | Backup and DR | Documented RPO/RTO for the ledger, backup and restore runbooks, migrations with `goose` | TS | P1 | + A§5 |
| OPS-08 | Kubernetes operator | `AIResource` CRD reconciled into the registry | OPT | P3 | = D§7.2 |
| OPS-09 | Multi-region | Regional edge + Valkey, global budget allocation, read replicas; only usage batches cross regions | DF | P3 | = D§14.3 |
| OPS-10 | Version compatibility | N−1 compatibility between edge and control; snapshot and canonical-schema versioning for mixed rollouts | EN | P2 | + A§5 |
| OPS-11 | CLI | `gwctl`: apply manifests, lint and dry-run policies, manage keys, export reports | TS | P1 | = D§7.2 (config-as-code) |
| OPS-12 | Performance validation | k6 scenarios on a realistic configuration (Tier 0 + ledger + TLS on both sides); targets re-baselined (section 18) | EN | P1 | ~ A§6 |

## 17. Console and self-service (CON)

| ID | Feature | What it does | Class | Phase | Origin |
|---|---|---|---|---|---|
| CON-01 | Admin console | Embedded SvelteKit app: tenants, projects, keys, budgets, usage, resources (P1); teams, principals, policies, guard profiles, aliases, price sheets (P2) | TS | P1 / P2 | = D10 · ~ A§3.3 (scope) |
| CON-02 | Tenant self-service portal | Create projects and keys within quotas, see usage and cost, request access to resources | TS | P3 | = D§5 |
| CON-03 | Playground | Test aliases and tools with a visible policy trace and score breakdown | TS | P2 | = D§5 |
| CON-04 | Policy simulator | Dry-run a request: which rules fired, effective constraints, chosen resource and why | TS | P2 | + D§18 (dry-run) · M: OpenRouter (breakdown) |
| CON-05 | Approval queues | Resource registration, tool-definition changes and human-in-the-loop tool calls | DF | P2 / P3 | + MCP-03, GRD-10 |
| CON-06 | Request log viewer | Redacted request and response browsing with replay, permissioned per role | TS | P2 | = D§15 |

---

## 18. Non-functional targets (revised, to validate with k6)

| Attribute | v0.1 target | Revised target |
|---|---|---|
| Added latency, fast path | p50 < 1 ms, p99 < 5 ms | Keep for pure passthrough; add a **realistic profile** (Tier 0 guards + ledger + TLS both sides): p50 < 2 ms, p99 < 10 ms |
| TTFT overhead with Tier 0 | < 3 ms | Keep, specified for prompts ≤ 4k tokens and a bounded regex set |
| Tier 1 guard latency | 5–15 ms in-process | ≤ 15 ms p95 for ≤ 2k tokens from the guard service, per-route budget; fallback mode on budget miss |
| Concurrent streams per 2 vCPU / 1 GB pod | ~5 000 | Two profiles: passthrough (target ~5 000) and output-guarded (measure, expect ~2 000); publish both |
| Idle RSS per edge pod | < 100 MB | Keep (pure-Go edge, no ONNX) |
| Binary / image size | < 60 MB / < 80 MB | Keep for the edge image |
| Config propagation | Rebuild < 200 ms | Full rebuild < 200 ms for ≤ 10k keys and 1k policies; incremental key, health and budget updates < 50 ms; cluster-wide propagation < 1 s; **key revocation < 1 s** |
| Quota accuracy | Overshoot ≤ lease size | P1: no overshoot beyond in-flight reservations; P3: ≤ configured lease size |
| Ledger durability | not stated | Zero event loss on pod crash; only a node disk loss can lose events, bounded by the spool flush interval |
| Control-plane outage | not stated | Edges keep serving from the last snapshot for ≥ 24 h; defined degraded-mode behaviour for budgets |
| Discovery evaluation | < 1 ms | Keep |

---

## 19. Explicitly out of scope or dropped

| Item | Status | Reason |
|---|---|---|
| Model training and fine-tuning orchestration, vector-database hosting, agent runtime | Out of scope (unchanged) | D§1 |
| argon2id for API-key verification | Dropped | Slow and memory-hard per request; DoS vector (A§4.1) |
| PostgreSQL `LISTEN/NOTIFY` as the propagation mechanism | Dropped | Not durable; grows DB connections with HPA (A§4.3) |
| In-process ONNX on the edge as the default | Dropped | Breaks static binary and memory targets; Gravitee's ~300 MB heap confirms it (A§4.2, M) |
| "Lossless" dialect translation | Dropped as a claim | Responses-API state, thinking signatures and server-side tools do not map (A§4.4) |
| MCP "OAuth passthrough" | Dropped | Forbidden by MCP security guidance; replaced by MCP-06 |
| "Rust rewrite of the edge" open decision | Dropped | Go is not the bottleneck in front of LLMs (A§3.4) |
| Cost normalisation across resource kinds | Dropped | Comparing ASR per second with LLM per token is not meaningful (A§4.10) |
| Traffic-dependent computed local cost | Changed | Feedback loop; capacity-normalised instead (A§4.10, BRK-05) |
| Own SDKs, semantic cache, realtime, stateful Responses, operator, A2A, multi-region | Deferred to P3 or later | Table stakes or optional; not on the critical path to the differentiators |

---

## 20. Open decisions that gate features

| Decision | Options | Features affected | Current lean |
|---|---|---|---|
| Internal platform vs multi-tenant product | internal · product · both | TEN-08 cells, CON-02 portal, LED-04 depth, licensing, security bar | Decide first; "both" is the most expensive |
| Adapter sourcing (ADP-05) | A: own binary + `bifrost/core` · B: Agent Router data plane · C: own adapters · D: buy and build only the broker | ADP-01…04, OPS-01 (B breaks the single binary), the P1 timeline | A by default; B if Envoy Gateway is already standard; D if internal-only |
| Default tool-call mode | pass-through · brokered | MCP-08, GRD-03, QTA-06 | Pass-through default, brokered per route |
| Guard service placement | separate service · in-process | GRD-04, OPS-01 | Separate by default, in-process only in `single` mode |
| Tenancy exposure in P1 | tenant/project/key only · all five levels in tooling | TEN-01, CON-01 | Schema for all five, tooling for three |
| Licensing and naming | — | distribution, community, sovereignty positioning | Open |
