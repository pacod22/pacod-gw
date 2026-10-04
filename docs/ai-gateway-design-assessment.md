# AI Gateway design v0.1 — critical assessment

| Field | Value |
|---|---|
| Subject | `ai-gateway-design.md` v0.1 (concept, draft, 2026-10-03) |
| Assessed | 2026-10-03 |
| Basis | The document only. No code, benchmarks or requirements docs exist yet. Statements about other products reflect public information up to mid-2026 and should be re-checked before deciding. |

## 1. Verdict in one paragraph

The architecture is competent and mostly coherent: the control/data-plane split, immutable snapshots, CEL on the hot path, tools as governed first-class resources, and lease-based quotas with an explicit error bound are all sound, well-known patterns. The document's weaknesses are less in the boxes and more around them. The biggest risks are strategic: the "build everything" decision is justified by preference rather than evidence, the product question (internal platform vs. multi-tenant product) is still open even though it drives most of the design, and the Phase 1 scope does not fit the stated team and timeline. Beneath that sit about ten concrete technical defects (argon2id on API keys, cgo breaking the "static binary" claim, lossy NOTIFY propagation, the "lossless translation" promise, MCP token passthrough and session state vs. stateless edges, undefined streaming-guard semantics, and a non-durable billing ledger) that are all fixable in a v0.2 of the document before any code is written.

| Area | Rating | One-line reason |
|---|---|---|
| Architecture fundamentals | Strong | CP/DP split, snapshots, async observability, clear package layout |
| Strategic justification (D1 greenfield) | Weak | Differentiators now exist in open-source gateways; no gap analysis |
| Requirements / users | Missing | No scenarios, SLOs per user type, or non-goals beyond v1 exclusions |
| Scope vs. team | Unrealistic | Phase 1 is roughly 2× what 3–4 engineers ship in 3 months |
| Hot-path engineering | Over-optimised | µs targets in front of calls that take seconds; correctness traded for speed |
| Security | Partial | Good controls listed, no threat model; several MCP-specific gaps |
| Content control | Optimistic | Classifiers treated as controls; architectural injection defences missing |
| Capability broker | Unproven value | Elegant, but dynamic ranking conflicts with how apps actually pin models |
| Operability | Mostly good | Simple deps; config propagation and ledger durability need rework |

## 2. Strengths worth keeping

1. **Control/data-plane split with immutable snapshots.** Atomic pointer swap of a compiled config is the right model (it is essentially what Envoy/xDS does) and makes the data plane easy to reason about and test.
2. **Tools as first-class, governed resources (D7).** Correctly identifies tool results as the main injection surface. Most gateways still treat MCP as an afterthought.
3. **Lease-based quotas with an explicit overshoot bound (D9).** Honest about the trade-off and makes it configurable per budget.
4. **CEL over Rego (D5).** Right call for hot-path evaluation; cel-go also supports static cost estimation, which matters for tenant-authored policies (see 4.6).
5. **Lean dependency footprint (D4).** PostgreSQL + Valkey in cluster mode, SQLite in single mode, is operable by a small on-prem team.
6. **Tenancy hierarchy with inheritance and "children cannot loosen hard caps".** Matches how enterprises actually allocate budget.
7. **Residency as a hard filter**, fail-closed policy errors, observability that never blocks requests.
8. **Adapter conformance suites with recorded fixtures.** Essential for the translation layer and often skipped.
9. **Document hygiene.** Decision log, open decisions, targets marked "to be validated". This makes the design reviewable.

## 3. Strategic issues (highest impact)

### 3.1 Build vs. extend is decided on preference, not evidence (D1)

The stated differentiator is "MCP brokering + content policy + tenancy hierarchy + capability discovery in one lean binary". As of mid-2026, most of that exists in open source:

| Project | Language | Overlap with this design |
|---|---|---|
| agentgateway (Linux Foundation, originated at Solo.io) | Rust | MCP + A2A gateway, LLM routing, **CEL-based authorisation**, multi-tenancy, xDS config, Gateway API |
| Envoy AI Gateway (Envoy/CNCF) | Go control plane + Envoy | Token-based rate limits, provider fallback, MCP support added in 2025 |
| Bifrost (Maxim AI) | Go | Very low overhead, virtual keys, budgets, governance, MCP |
| LiteLLM | Python | Broadest provider coverage, teams/orgs, budgets, guardrail hooks, MCP gateway; weaker at high RPS |

What genuinely seems unique is the **capability broker** (registration, observed-metric ranking, computed local cost) and **sovereignty-oriented tenancy**. Those are control-plane features. Building a new data plane, six provider adapters, an MCP client/server, streaming guards and a console to get there is the expensive way to reach the differentiator.

The largest hidden cost is not the initial build but **adapter maintenance**: OpenAI, Anthropic and Google change request/response formats, tool-call shapes, reasoning/thinking blocks and caching semantics every few weeks. Expect that to consume roughly one engineer permanently.

**Recommendation:** run a two-week, time-boxed spike that scores agentgateway, Envoy AI Gateway and Bifrost against the five must-haves (whatever 3.2 produces). Either build the broker and tenancy control plane on top of an existing data plane, or keep greenfield but replace D1's rationale with the concrete gaps found. Both outcomes are fine; the current rationale ("full control") is not a reason a reviewer or funder can check.

### 3.2 The product question is open but drives everything

Open decision "Internal vs. product" determines tenancy depth, sovereignty, self-service portal, contract prices, billing-grade ledger accuracy, licensing and the security bar (internal teams vs. mutually distrusting customers). The design currently builds for both, which is the most expensive option.

**Recommendation:** decide this first and add a short requirements section: 3–5 concrete user scenarios (e.g. "an internal team ships a support bot using EU-only models with a monthly budget"), who the first users are, the measurable success criteria, and explicit non-goals.

### 3.3 Phase 1 scope does not fit 3 months × 3–4 engineers

Phase 1 contains two northbound dialects, three adapter families with conformance suites, OIDC + virtual keys, the full tenancy hierarchy, CEL policies, distributed lease quotas, the ledger, Tier 0 guards, a registry with two registration paths, discovery, console v1, Helm charts for two modes, and k6 validation. That is 9–12 person-months of budget for something closer to 20.

Worse, Phase 1 **excludes the differentiator** (MCP broker is Phase 2). After Phase 1 you have a less mature LiteLLM/Bifrost.

**Recommendation:** re-cut Phase 1 as a thin vertical slice that includes the differentiator:

- OpenAI-compatible and Anthropic **same-dialect passthrough only** (no cross-dialect translation yet)
- Virtual keys, tenant → project only (two levels), project budgets with simple Valkey counters
- Tier 0 guards, ledger, Prometheus/OTel
- MCP virtual server with tool scopes and tool-definition pinning (see 4.5)
- YAML registry, static aliases, no ranking
- Minimal console (keys, budgets, usage) or CLI only

Defer cross-dialect translation, lease-based quotas, discovery ranking and bindings, computed local costs, semantic cache, own SDKs.

### 3.4 The hot path is over-optimised for the wrong constraint

LLM calls have TTFT of hundreds of milliseconds to seconds and run for seconds to minutes. A 1 ms vs. 5 ms gateway overhead is noise to users. What actually limits a gateway in production is **correctness** (billing, policy, tool safety), **memory per long-lived stream**, connection handling, and upstream rate limits.

Several decisions trade correctness or simplicity for microseconds:

- The principle "no I/O on the hot path except the upstream call" is already broken by the design itself (strict Valkey check near exhaustion, Valkey exact cache, inline Tier 2, brokered tool calls). Restate it as "no synchronous **configuration** I/O on the hot path".
- Lease-based quotas (D9) solve a problem a single in-cluster Valkey `EVALSHA` (~0.1–0.3 ms) does not have at v1 scale. Strict counters are simpler and exact. Keep leases as a later optimisation for multi-region.
- The "Rust rewrite of edge later" open decision should be removed; Go is not going to be the bottleneck in front of LLMs.

## 4. Technical issues

### 4.1 argon2id for API-key verification is a defect

argon2id is deliberately slow and memory-hard (typically tens of ms and tens of MB per verification). It exists to protect **low-entropy passwords**. Virtual keys are high-entropy random secrets; brute force is not the threat. Verifying argon2id per request is a CPU/memory sink and a trivial DoS vector (spray invalid keys).

**Fix:** store `HMAC-SHA256(pepper, key)` and look it up directly in the snapshot (O(1) map). Keep argon2id only if console users have local passwords (they should not; identity is OIDC).

### 4.2 "Single static binary" is contradicted by the stack

- `onnxruntime_go` needs cgo plus the ONNX Runtime shared library.
- `go-duckdb` needs cgo.
- Tier 1 models (DeBERTa-class injection classifier, GLiNER-class NER) are hundreds of MB each and, on CPU, contend with 5 000 streams on a 2 vCPU pod. DeBERTa's 512-token window means long prompts are chunked, so latency scales with prompt length and the 5–15 ms figure only holds for short inputs.

This also breaks the targets "binary < 60 MB", "idle RSS < 100 MB" and "5 000 streams per 1 GB pod" whenever Tier 1 is enabled.

**Fix:** keep the edge pure Go and static. Make Tier 1 a separate, horizontally scaled **guard service** (CPU or GPU pool) called over gRPC with a latency budget; the "optional sidecar" becomes the default. Move DuckDB reporting to the control role only, or to a separate `report` role, and accept cgo there.

### 4.3 Config propagation via PostgreSQL LISTEN/NOTIFY is fragile

- NOTIFY is not durable: notifications sent while an edge is reconnecting are lost silently.
- Every edge needs a direct PostgreSQL connection, which contradicts "control plane owns configuration", grows DB connections with HPA, and complicates multi-region.
- Health changes trigger a **full** snapshot rebuild on every edge. With flapping resources or a large key set, "rebuild < 200 ms" will not hold, and rebuild storms are likely.

**Fix:** have the control plane build versioned snapshots and push them (or deltas) to edges over a gRPC stream (an xDS-style pattern); edges ACK the version and resync fully on reconnect. Split fast-changing state (health, key revocations, budgets) from slow config (policies, registry, prices) so health flaps do not recompile CEL.

### 4.4 "Lossless" dialect translation is not achievable

Examples that do not map losslessly:

- OpenAI **Responses API** is stateful (`previous_response_id`, server-side conversation, background mode, built-in tools). Translating it to a stateless provider requires the gateway to store conversation state, which the design never mentions.
- Anthropic **thinking blocks** carry signatures that must be returned verbatim; Gemini has its own thought signatures. They cannot be converted between vendors.
- Prompt-caching semantics differ (explicit `cache_control` breakpoints vs. automatic prefix caching).
- Server-side tools (web search, code execution, citations, provider MCP connectors) exist only on specific providers.
- Realtime/voice APIs over WebSocket are a different protocol class, not just "streaming for all dialects".

**Fix:** drop "lossless"; define a feature matrix per adapter and fail loudly (`400 unsupported_feature`) rather than silently dropping fields. Make the canonical schema primarily a **metadata extraction** layer (usage, tool calls, content for guards) and keep same-dialect passthrough as the default route, with translation opt-in per alias.

### 4.5 MCP brokering has several unaddressed hard problems

- **"OAuth passthrough" is an anti-pattern.** The MCP security guidance explicitly forbids token passthrough (confused-deputy risk). The gateway needs a per-user **credential vault** with its own consent flows and token exchange (on-behalf-of) for upstream servers. This is a significant missing component.
- **Session state vs. stateless edges.** Streamable HTTP uses `Mcp-Session-Id`, server-to-client requests (sampling, elicitation, roots) and notifications (`tools/list_changed`). An aggregating virtual server across stateless edges needs session affinity or a shared session store.
- **Tool poisoning and rug pulls.** Tool descriptions are injected into model context. Self-registered or introspected servers can ship malicious descriptions, or change them after approval. Pin a hash of each tool definition at approval time and require re-approval on change.
- **Tool-count explosion.** A virtual server exposing every permitted tool floods context and degrades model accuracy. Scope tool lists per project/route by default.
- **stdio servers in sandboxed pods** need a stdio-to-HTTP bridge, and per-user servers holding user credentials imply per-user pods. Lifecycle, scale-to-zero and cost are undefined.

### 4.6 Policy semantics are underspecified

- Composition rules across hierarchy levels are not defined. Is it deny-overrides? Most-specific-wins? How are `constraint` maps merged? "Hard caps cannot be loosened" needs a precise definition (a lattice per attribute).
- The `constraint` example returns a map literal from CEL, which uses CEL as a config language. Consider plain typed config for constraints and CEL only for predicates.
- If tenants can author policies (self-service, Phase 3), enforce cel-go cost limits at compile time, or one tenant's expensive expression degrades everyone.

### 4.7 Streaming output guard semantics are undefined

Once tokens are sent they cannot be recalled. The document needs to specify:

- holdback window size and its TTFT cost per guard profile;
- what "block" means mid-stream (terminate with an error event; what the client sees; whether usage is still billed);
- how redaction interacts with tool-call and JSON deltas (redacting inside a partial JSON argument breaks it). Buffer fully for structured output and tool calls;
- whether retries or fallbacks are allowed after partial output (generally not).

### 4.8 Quota model edge cases

- Output tokens are unknowable at reservation time. Reasoning models can burn tens of thousands of hidden tokens; reserving `max_tokens` is pessimistic, and estimates underrun. Define a mid-stream cutoff policy when a hard budget is hit.
- A request must reserve against key, project, team and tenant budgets simultaneously; with leases that is four leases per request and four sources of overshoot.
- "2 % or 10 s worth" leases fragment badly for small budgets across many pods (a €50 project budget over 20 pods).

### 4.9 The ledger is "source of truth" but is not durable

Usage events go through an in-memory ring buffer; a pod crash or OOM loses up to 1 000 events, and a PostgreSQL outage overflows the buffer. That is acceptable for metrics, not for chargeback.

**Fix:** decide the drop policy explicitly. For billing-grade accuracy, spool to a local append-only file (or NATS JetStream) before acknowledging, and reconcile against provider invoices/usage APIs monthly.

### 4.10 Capability broker: elegant, value unproven

- Apps tune prompts to a specific model. A dynamically ranked alias that silently switches between models changes outputs, so most production callers will pin. Validate demand with real users before investing in ranking and signed bindings.
- Cost normalisation "across kinds" (ASR per second vs. LLM per token) is not meaningful; rank only within a kind.
- Computed local cost (GPU €/h ÷ measured throughput) creates a feedback loop: more traffic makes a model look cheaper, which attracts more traffic. Use capacity-normalised cost or a fixed rate.
- Observed p95 latency depends on prompt and output length; normalise to TTFT and per-token latency.
- Vendor MMLU-Pro scores are weak quality signals; plan for task-specific evals if quality ranking matters.
- Routing across replicas destroys provider and vLLM **prefix-cache hit rates**, which often dominates cost. Add session/prefix affinity to routing.

### 4.11 Security gaps

There is no threat model, and the gateway is the crown jewel: it holds every provider key and sees every prompt.

- **Blast radius.** Every edge snapshot holds all tenants' decrypted provider keys. One compromised edge leaks everything. For sovereign tenants, offer dedicated edge pools (cells) with only their keys.
- **Key management.** `age` encryption is only as strong as where the age identity lives, likely a Kubernetes Secret next to the ciphertext. Use KMS/Vault/OpenBao envelope encryption.
- **SSRF.** Introspection probes and the OpenAPI tool wrapper call admin-supplied URLs from inside the cluster. Enforce egress allowlists and block link-local/cluster CIDRs.
- **Bypass.** Appendix A's vLLM uses `auth: none`. Any pod can call it directly and skip the gateway. Add ingress NetworkPolicies on model servers allowing only edge pods.
- **Residency is more than routing.** Traces, logs, replay store, Valkey exact cache (which stores responses) and a single-primary PostgreSQL ledger all hold tenant data. Multi-region must keep these regional too.
- **Privacy and retention.** "Replay from stored canonical request" stores prompts. Define GDPR erasure across ledger partitions and Parquet exports, which are hard to delete from.
- **Admin surface.** Ensure `--role edge` does not serve console or admin routes.
- **Agent identity.** Agents acting for users need delegated authority (token exchange, RFC 8693), not just a "principal type: agent".

### 4.12 Prompt-injection defence leans too hard on classifiers

DeBERTa-class injection classifiers have meaningful false-positive rates and are bypassed by adaptive attacks. They are useful signals, not controls. The strongest defences are architectural:

- **Taint tracking per session.** Once untrusted content (tool results, fetched pages) enters the context, restrict subsequent tool calls (e.g. no outbound-send or write tools without approval). This addresses the "private data + untrusted content + exfiltration channel" combination directly.
- **Human approval** for sensitive tool calls (MCP elicitation is a natural fit).
- **Least-privilege tool scopes** per project, which the design already has.

Add these to section 9 and treat Tier 1/2 as detection and audit.

## 5. Missing items

| Gap | Why it matters |
|---|---|
| Upstream rate-limit management (per-provider key pools, TPM/RPM awareness, spillover on 429) | The most common production failure mode for gateways |
| Provider prompt-cache and prefix affinity | Large cost lever; current load balancing works against it |
| Provider batch APIs (≈50 % discount) | Only "native batch" is mentioned |
| Long-running requests (reasoning, background mode, 10+ minute streams) | Timeouts, HTTP/2 limits, pod drain during rollouts |
| Mixed-version rollouts | Snapshot and canonical-schema compatibility between old and new edges |
| End-user and custom metadata tags on requests | Needed for real chargeback and abuse tracing |
| Backup, DR and RPO/RTO for PostgreSQL | Ledger is billing data |
| Synthetic canary cost | Canaries against paid and reasoning models cost money; budget them |
| Maintained libraries | `gojsonschema` is unmaintained (consider `santhosh-tekuri/jsonschema`); pin a current Go release rather than 1.23 |

## 6. Engineering targets: realism check

| Target | Assessment |
|---|---|
| Fast path p50 < 1 ms / p99 < 5 ms | Achievable in Go, but the fast path rarely applies: "tool scopes and result guards are mandatory" and most tenants will run Tier 0. Measure with Tier 0, ledger and TLS on both sides. |
| TTFT overhead with Tier 0 < 3 ms | Plausible for short prompts with a bounded regex set |
| 5 000 streams per 2 vCPU / 1 GB | ≈200 KB per stream for two TLS connections, buffers, goroutines and guard window. Plausible only for passthrough without output guards. |
| Idle RSS < 100 MB | Fails with ONNX in-process or a large key set in the snapshot |
| Snapshot rebuild < 200 ms | Fails at scale with full rebuilds on every health change (see 4.3) |
| Phase 1 in 3 months with 3–4 engineers | Not with the current scope (see 3.3) |

## 7. Prioritised recommendations

| # | Action | Effort | Impact |
|---|---|---|---|
| 1 | Decide internal vs. product; add 3–5 user scenarios, success criteria and non-goals | Days | Highest: unblocks scoping |
| 2 | Two-week build-vs-extend spike against agentgateway, Envoy AI Gateway and Bifrost; rewrite D1 with the result | 2 weeks | High: may save months |
| 3 | Re-cut Phase 1 into a vertical slice that includes MCP governance; push ranking, translation, leases and semantic cache later | Days | High |
| 4 | Fix the concrete defects: HMAC key lookup, pure-Go edge with a separate guard service, pushed versioned snapshots, durable ledger spool | Doc changes now | High |
| 5 | Write a threat model (provider-key blast radius, SSRF, tool poisoning, cross-tenant, insider/admin, bypass) | 1 week | High |
| 6 | Specify MCP auth (no passthrough, credential vault, OBO), sessions, tool pinning and approval | 1 week | High: the differentiator depends on it |
| 7 | Replace "lossless translation" with a per-adapter feature matrix and fail-loud behaviour; make passthrough the default | Days | Medium |
| 8 | Add architectural injection controls (taint tracking, approvals) and reframe classifiers as signals | Days | Medium |
| 9 | Define policy composition semantics, streaming-guard semantics and quota cutoff behaviour | Days | Medium |
| 10 | Re-baseline targets against a realistic configuration and budget one engineer for ongoing adapter maintenance | Days | Medium |

## 8. Suggested v0.2 document changes

- Add **Requirements** (scenarios, users, SLOs, non-goals) before section 1.
- Add **Threat model** next to section 16.
- Rewrite D1 (evidence), D9 (strict counters first), D11 (guard service default) and principle 2 ("no synchronous config I/O").
- Replace the NOTIFY propagation paragraph with a versioned push design.
- Expand 8.1 and section 9 with MCP auth/session design and streaming-guard semantics.
- Move `/v1/responses` out of v1 or specify how its state is stored.
- Re-plan section 20 and close the "Rust rewrite" open decision.
