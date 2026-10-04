# LLM usage in PACOD

How the platform uses large language models end to end. It covers the LiteLLM gateway, model and key management, the per-role runtime configuration, reliability (timeouts and retries), token and cost accounting, pricing, quotas, metrics and alerting, analytics and reporting, and debugging.

_Last revised: 2026-10-04. This replaces `LITELLM_USAGE.md` (the 2026-10-03 LiteLLM-only analysis). Sections 2–4 carry that analysis forward._

---

## TL;DR

- **One gateway.** Every LLM call goes through the LiteLLM **proxy** (`http://litellm:4000`), using the `openai` SDK. The `litellm` Python package is not a dependency, and no code calls a provider directly.
- **LiteLLM is a thin layer.** It handles provider abstraction, holds the provider keys, normalises responses, drops unsupported params, and caps timeouts and retries. Everything else is built into pacod. That covers cost, quotas, model catalogue, metrics, analytics and debugging.
- **Models are chosen per role, per profile, at runtime.** Each LLM role (planner, synthesizer, loop judge, vision, …) gets either an **LLM template** or the legacy flat fields. Choices come from one resolver, `available_models()`. No restart is needed except to add a model to the gateway.
- **Every call is accounted.** A per-turn `UsageAccumulator` produces `usage_events` rows (tokens, cached tokens, USD cost, call count), plus Prometheus counters. Failed calls go to `llm_call_errors`. Cost comes from the admin-managed `model_prices` table, which a daily web-search + LLM job keeps up to date.
- **Quotas are per user.** Token and/or request quotas, each with a configurable period, plus a per-minute throttle. All of it lives in Redis and can be changed at runtime.
- **Where to look:**
  - Admin **LLM & Search → Usage & costs**: spend and tokens.
  - **Monitoring → Analytics**: usage per site, profile and user, plus errors.
  - Grafana **pacod — Usage**: live rates.
  - **Debug Console**: one turn's prompts and answers.
  - `llm_call_errors`: why calls failed.

| Concern | Primary mechanism | Section |
|---|---|---|
| Gateway / provider routing | LiteLLM proxy | [2](#2-the-litellm-gateway) |
| Model catalogue, templates, per-role selection | `llm_models`, `llm_templates`, `app_settings` | [5](#5-model-management) |
| Provider keys | `provider_secrets` → `providers.env` | [6](#6-provider-key-management) |
| Timeouts / retries | client + proxy, never retry a timeout | [7](#7-reliability-timeouts-and-retries) |
| Tokens & cost | `UsageAccumulator` → `usage_events`, `model_prices` | [8](#8-token-and-cost-accounting) |
| Pricing | `model_prices` + daily price-refresh job | [9](#9-pricing-management) |
| Cost levers | prompt cache, response cache, routing cheap models | [10](#10-cost-and-token-levers) |
| Quotas / rate limits | Redis fixed windows + sliding throttle | [11](#11-quota-management) |
| Metrics & alerts | Prometheus `llm_*` counters, Grafana, Alertmanager | [12](#12-metrics-monitoring-and-alerting) |
| Analytics & reporting | admin Usage & costs, Analytics, loop reports | [13](#13-analytics-statistics-and-reporting) |
| Debugging | Debug Console, turn evidence, error log, structlog | [14](#14-debugging-llm-behaviour) |

---

## 1. Architecture at a glance

```
             ┌───────────── admin-api ──────────────┐
             │ llm-config / llm-models / templates  │──writes──► app_settings, llm_models,
             │ model-prices / quota-config / keys   │            llm_templates, model_prices,
             └──────────────┬───────────────────────┘            provider_secrets
                            │ renders                                   │
                            ▼                                           │ read per turn
       docker/litellm/secrets/{managed-models.yaml, providers.env}      │ (Redis-cached)
                            │ (applied on `make apply-llm-*`)           ▼
 api / improvement-loop-worker / newsletter-worker ──► LLMClient ──► LiteLLM proxy ──► Anthropic, OpenAI,
   guards (quota) → runtime config → template params      │                           Gemini, DeepSeek, GLM
                                                          ├─► UsageAccumulator → usage_events (+ cost)
                                                          ├─► Prometheus llm_* counters (api/admin-api only)
                                                          ├─► llm_call_errors (final failures)
                                                          └─► debug_interactions (when debug audit is on)
```

**LLM call sites** and their usage `purpose` tags:

| Area | Purposes | Where |
|---|---|---|
| Chat turn | `classifier` (topic guard), `planner`, `synthesizer` or `synthesizer_draft` + `verifier`, `suggestions`, `summarizer` (conversation memory) | `app/guards/topic.py`, `app/agent/graph.py`, `app/services/conversation_memory.py` |
| Fortune Teller | `reading_vision`, `reading_tts` | `app/readings/interpret.py`, `app/readings/tts.py` |
| Review process | `loop_triage`, `loop_analysis` (A and B), `loop_judge`, `loop_critic`, `loop_remediation`, `loop_facts`, `loop_cluster`, `loop_entailment` | `improvement_loop/` (via `improvement_loop/llm.py`) |
| Newsletter | `newsletter` | `app/services/newsletter_classify.py` |
| Price refresh | `price_fetch` | `app/services/price_fetch.py` |
| Agent editor assistant | `agent_editor_assistant` | `app/agent_editor/assistant.py` |
| Web search (not an LLM) | `web_search` (model = `tavily`/`google`, estimated cost) | `app/services/web_search.py` |

---

## 2. The LiteLLM gateway

### 2.1 Deployment topology

| Item | Value | Where |
|---|---|---|
| Image | `ghcr.io/berriai/litellm:main-latest` (unpinned) | `docker-compose.yml` → `litellm` service |
| Command | `--config /app/config.yaml --port 4000 --num_workers 2` | same |
| Config | `docker/litellm/config.yaml` (read-only bind mount) | |
| Managed model list | `docker/litellm/secrets/managed-models.yaml` (via `include:`) | generated by admin-api |
| Provider keys | `.env` + optional `docker/litellm/secrets/providers.env` (`env_file`, `required: false`) | generated by admin-api |
| Network | internal `agent-net` only; not exposed publicly | |
| Clients | `api`, `admin-api` (model registry probe only), `improvement-loop`, `improvement-loop-worker`, `newsletter-worker` | `LITELLM_URL=http://litellm:4000` |
| Auth | single `LITELLM_MASTER_KEY`, sent as a Bearer token by every client | `.env`, `app/core/config.py` (`litellm_url`, `litellm_master_key`) |

The proxy runs **without a database**: no `DATABASE_URL` and no `store_model_in_db`. The LiteLLM admin UI, virtual keys, teams, spend logs and DB-stored models are therefore unavailable, and none of them are used.

### 2.2 Model routing / provider abstraction (`model_list`)

One OpenAI-compatible endpoint fronts five providers. The `model_name` is the id the app uses everywhere: usage events, prices, per-role selection. `litellm_params.model` is what LiteLLM sends upstream.

| Provider | Model names (baseline) | LiteLLM routing |
|---|---|---|
| Anthropic | `anthropic/claude-sonnet-5-5`, `anthropic/claude-opus-5`, `anthropic/claude-haiku-4-5` | native `anthropic/` adapter |
| OpenAI | `openai/gpt-5.6-sol`, `openai/gpt-5.6-luna`, `openai/gpt-4o-mini-tts` | native `openai/` adapter |
| Google | `gemini-3.1-pro-preview`, `gemini-3.8-flash`, `gemini-3.5-flash-lite` (bare aliases) | `gemini/…` (AI Studio) adapter |
| DeepSeek | `deepseek/deepseek-flash`, `deepseek/deepseek-v4-pro` | native `deepseek/` adapter |
| GLM (Zhipu/z.ai) | `glm/glm-5.3`, `glm/glm-4.7`, `glm/glm-4.5-air` | **`openai/` adapter + custom `api_base`** (OpenAI-compatible endpoint `GLM_API_BASE`, default `https://api.z.ai/api/paas/v4`) |

Features used:
- **Model aliasing**: the alias differs from the upstream id (e.g. `glm/glm-5.3` → `openai/glm-5.3`; `gemini-3.8-flash` → `gemini/gemini-3.8-flash`).
- **`os.environ/NAME` secret references** for `api_key` and `api_base`.
- **`include:`** merges a generated file (`/secrets/managed-models.yaml`) into `model_list`. The proxy won't boot if that file is missing, so `make ensure-llm-models` always creates a valid empty one.

### 2.3 Proxy settings

| Section | Setting | Value | Why it matters |
|---|---|---|---|
| `litellm_settings` | `drop_params` | `true` | Silently drops params a provider rejects. The app depends on this: Anthropic `thinking` sent to non-Anthropic models, `cache_control` sent to non-caching providers, and so on. |
| | `request_timeout` | `600` | Upstream timeout per call. Must be ≥ `LLM_TIMEOUT_SECONDS` (180), `LOOP_LLM_TIMEOUT_SECONDS` (600) and every template's `timeout_seconds` (capped at 600). |
| | `num_retries` | `2` | Proxy-side retry of transient failures (429/5xx/connection). |
| | `telemetry` | `false` | Disables LiteLLM's own telemetry. |
| `router_settings` | `retry_policy.TimeoutErrorRetries` | `0` | A timed-out call is **never re-sent**, because it may already have completed and been billed upstream (the gpt-5.6 double-billing incident). |
| `general_settings` | `master_key` | `os.environ/LITELLM_MASTER_KEY` | The only auth mechanism. |

### 2.4 HTTP endpoints called on the proxy

| Endpoint | Caller | Purpose |
|---|---|---|
| `POST /v1/chat/completions` (non-streaming) | `LLMClient._chat_once` / `chat` / `chat_json[_meta]` (`app/services/llm_client.py`) | planner, classifiers, review loop, newsletter, price extraction, memory summariser, vision readings, editor assistant |
| `POST /v1/chat/completions` (`stream=True`, `stream_options.include_usage=True`) | `LLMClient.chat_stream` | the synthesizer's user-facing streamed answer |
| `POST /v1/audio/speech` | `app/readings/tts.py` (`openai/gpt-4o-mini-tts`, mp3) | text-to-speech for Baba Vanga readings |
| `GET /v1/models` | `app/services/llm_models.gateway_models()` via `httpx` | live registry of the models the proxy can serve right now |

**Not called:** `/model/info`, `/health*`, `/key/*`, `/spend/*`, `/user/*`, `/team/*`, `/embeddings`, `/images/*`, `/audio/transcriptions`, `/moderations`, `/rerank`, `/metrics`. Embeddings go to the separate `embeddings` service, not through LiteLLM. `/utils/transform_request` was used by hand to calibrate the capability table (§5.4).

---

## 3. Request/response features relied on (cross-provider translation)

These are the OpenAI-format features that LiteLLM translates per provider and the app depends on.

| Feature | How the app uses it | LiteLLM behaviour relied on |
|---|---|---|
| **Chat messages** (system/user/assistant, history) | `_build_messages`. History is normalised first (`conversation_memory._normalize`), because Gemini returns a 400 via LiteLLM on consecutive same-role turns or a leading assistant turn | message format translation |
| **`max_tokens`, `temperature`, `top_p`, penalties** | sampling params are stripped per model capability (`_sampling_params`, §5.4) | param mapping + `drop_params` |
| **Anthropic extended thinking** via `extra_body.thinking` | legacy Claude (Haiku 4.5): `{"type":"enabled","budget_tokens":N}`. Adaptive Claude: `{"type":"adaptive","display":"summarized"}` | Anthropic-native param pass-through |
| **Portable `reasoning_effort`** | sent when thinking is on. Adaptive Claude `low…max`, OpenAI reasoning `low…xhigh`, others `low/medium/high` (templates; the legacy default is `"high"`) | LiteLLM maps it to each provider's native control (incl. `thinking:adaptive` + `output_config.effort` for Claude) |
| **Reasoning stream normalisation** | reads `delta.thinking` (Anthropic) or `delta.reasoning_content` (Gemini/DeepSeek), also via `model_extra`, and wraps them in `<think>…</think>` for OpenWebUI when `thinking_tags=True` | LiteLLM's unified reasoning fields |
| **Prompt caching** | `build_cached_system()` sends OpenAI-style content blocks with `cache_control: {"type":"ephemeral"}` on the static system prefix | passed through to Anthropic; stripped for others by `drop_params` |
| **Cached-token reporting** | `usage.prompt_tokens_details.cached_tokens` → `TokenUsage.cached_tokens` | LiteLLM normalises Anthropic/OpenAI cache-read counts |
| **Usage on streams** | `stream_options={"include_usage": True}`. The final chunk carries `prompt_tokens`/`completion_tokens` | usage normalisation for all providers |
| **`finish_reason`** | `ChatReply.truncated` (`finish_reason == "length"`) in `chat_json_meta`. The loop retries once with a doubled budget | normalised finish reasons |
| **Tools / `tool_choice:"auto"`** | used **only** for Anthropic's native server-side web search tool (`web_search_20250305`, `max_uses=5`, `allowed_domains`) in `app/services/web_search_native.py` | pass-through of Anthropic server tools |
| **`provider_specific_fields`** | citations parsed from `delta.provider_specific_fields.web_search_results` (`extract_web_citations`) | LiteLLM surfaces Anthropic web-search result blocks here |
| **Multimodal input (vision)** | Fortune Teller: one non-streaming multimodal JSON call with image content blocks (`user_content`) | image-block translation per provider |
| **Audio speech** | `audio.speech.create(model, voice, input, response_format="mp3")` | OpenAI TTS pass-through |

Native web search was calibrated against the live proxy. **Anthropic: supported.** Gemini `googleSearch`: produced no usable streamed content through LiteLLM. OpenAI: not wired. The last two fall back to the external Tavily/Google search.

**Not used:** `response_format` / JSON mode / structured outputs (JSON is prompted and parsed by `parse_llm_json`, which strips code fences), application-defined function calling, `n>1`, logprobs, `seed`.

---

## 4. What pacod does instead of LiteLLM

| Concern | LiteLLM offers | PACOD does it itself (section) |
|---|---|---|
| Cost calculation | price map / `response_cost` / spend tracking | `model_prices` + `model_pricing.cost_for`, daily price-refresh job (§8–9) |
| Usage accounting | spend logs (needs DB) | `UsageAccumulator` → `usage_events` + Prometheus (§8, §12) |
| Budgets / quotas / rate limits | virtual keys, budgets, TPM/RPM | Redis token/request quotas + per-minute throttle (§11) |
| Retries / timeouts | `num_retries`, `request_timeout` | client tenacity + proxy, timeouts never retried (§7) |
| Error tracking | callbacks / alerting | `llm_call_errors` + `llm_requests_total{outcome}` + alert rules (§12, §14) |
| Observability | `prometheus` callback, Langfuse … | app Prometheus series, Grafana, structlog, Debug Console. The LiteLLM scrape is **disabled** (§12) |
| Model management | DB models + UI | `llm_models` catalogue → generated include file (§5) |
| Parameter presets | n/a | `llm_templates` (§5.2) |
| Key management | virtual keys / UI | `provider_secrets` (Fernet) → `providers.env` (§6) |
| Fallbacks / load balancing | router fallbacks, multiple deployments | **deliberately avoided**. A duplicate `model_name` would make LiteLLM load-balance (§5.1) |
| Response caching | Redis/semantic cache | own semantic response cache over bge-m3 embeddings (§10) |
| Guardrails / moderation | guardrail hooks | guard chain auth → profile → language → quota → topic, plus the ABSOLUTE RESTRICTION in prompts |
| Model retirement | n/a | `RETIRED_MODELS` boot-time remap (§5.5) |

---

## 5. Model management

Three layers decide which model a call uses and how it is called. All of them are admin-editable at runtime.

```
env defaults (AGENT_*_MODEL, LOOP_*_MODEL …)
  → app_settings flat fields per profile (agent.*_model, loop.*_model, reading.*_model, …)
    → LLM template assigned to the role (*_template)
      → published agent-editor overlay (owns the role if the version sets any of its flat fields)
        → remap_model (RETIRED_MODELS)
```

### 5.1 Global model catalogue (`llm_models`)

Implemented in `app/services/llm_models.py`, `admin_api/routes/llm_models.py`, and the admin page **Agent → LLM models**.

- **`available_models()`** is the single resolver behind every model dropdown and validation: (enabled catalogue rows ∪ the proxy's live `/v1/models`) − disabled rows. A newly added model is selectable *before* the gateway restart. `llm_config`, `model_prices`, `loop_config`, `newsletter`, `readings` and the price job all read it, so repoint them together if it ever changes.
- **Gateway export.** Enabled rows that are not static baseline ids are rendered by `render_litellm_models()` into `LITELLM_MODELS_FILE`. Key and base values are rendered as `os.environ/NAME`, never as key material. Apply with `make apply-llm-models`, which recreates the proxy. The page flags `in_gateway` per row and `pending_restart` overall.
- **Static ids are never re-exported.** A duplicate `model_name` would turn into a load-balancing group ("my change works half the time"), so static rows are flagged `static_config` and their wiring stays in `config.yaml`.
- **Disable, don't delete.** Deleting a row leaves the gateway still offering the model. Disabling hides it everywhere. `model_prices`/`usage_events` are never touched by catalogue deletes, so cost history survives.
- Seeded insert-if-absent at `api` boot from `config.yaml`. A new provider also needs its key wired in `provider_secrets._KEYS` (§6).

### 5.2 LLM templates (`llm_templates`, global)

Implemented in `app/services/llm_templates.py`, `llm_params.py`, `admin_api/routes/llm_templates.py`, and the admin tab **LLM & Search → Templates**.

- A template is a named parameter set: `model`, `max_output_tokens`, `temperature` (NULL = omit), `thinking_budget_tokens`, `reasoning_effort`, `top_p`, `frequency_penalty`, `presence_penalty`, `timeout_seconds` (≤ 600).
- **Every LLM role picks one** via an `app_settings` `*_template` key:
  - agent: `synthesizer`, `verifier`, `planner`, `classifier`, `suggestions`, `history_summary`, `newsletter`
  - loop: `triage`, `analysis_a`, `analysis_b`, `judge`, `remediation`
  - `reading.vision_template`
  - global `editor_assistant.template`

  Blank = **Legacy**: the role keeps its flat fields. TTS is not templated.
- **Live link.** Config caches store only template *ids*. Template rows come from one global cache (`config:llm_templates`, 60 s, busted on every template write), so a template edit reaches every profile at once.
- **One accessor per caller**: `AgentRuntimeConfig.call(role)`, `LoopConfig.stage(stage)`, `ReadingConfig.vision(runtime)`, `AssistantConfig.call()`. Each returns `LLMCallParams`. `extras()` omits unset knobs, so a legacy call is byte-identical to the pre-template one. To add a role, write its `legacy_*_params` function first.
- **Boot migration** (`llm_templates_migration.py`, once per deployment): every unassigned role gets a template built from its legacy params, deduped per role kind. Trade-off: a role on a template no longer follows `.env` model changes or the "blank → primary" fallback. Set it back to Legacy for that.
- Writes return 422 `unknown_model` / `unknown_template`. Deleting a template still in use returns 409 `template_in_use` with its usages. Editing a template the editor assistant uses requires superuser.

### 5.3 Per-role runtime config (per profile)

`app/services/agent_runtime_config.py` (`AgentRuntimeConfig`) is loaded **once per turn**, Redis-cached under `config:agent_runtime:{profile}`, and threaded on `AgentState.runtime`. It holds:
- the per-role model/template;
- temperature + the send-temperature toggle, max tokens per phase, thinking budget, prompt-cache flag;
- verification on/off, the `history_*` memory settings, and web search.

Edited at `PATCH /api/v1/llm-config?profile_id=…`, which takes effect on the next turn.

Sibling configs follow the same app_settings → Redis → env pattern:
- `loop_runtime_config.py`: Review process stages, read fresh per run.
- `app/readings/config.py`: vision model and TTS model/voice, edited in the Fortune Teller section of **LLM & Search**.
- `newsletter_runtime_config.py`
- `assistant_config.py`: agent editor assistant.

Model ids, provider keys and pricing are **global**. Only the *selection* is per profile.

### 5.4 Capabilities (`app/services/llm_capabilities.py`)

The table below says which knobs each model family accepts. `_sampling_params` strips the rest before the request is sent, and the API returns `capabilities` per model and `ignored_params` per template so the UI shows only valid knobs.

| Family | Temperature / top_p / penalties | Thinking | Effort |
|---|---|---|---|
| Adaptive Claude (Opus 4.7+/5, Sonnet 5/5.5, Fable 5, Mythos 5) | **rejected** (Anthropic 400s on `temperature`) | adaptive | `low…max` |
| Legacy Claude (Haiku 4.5) | accepted | `budget_tokens` ≥ 1024 | none |
| OpenAI reasoning (GPT-5.x, o-series) | dropped | n/a | `low…xhigh` |
| Gemini / DeepSeek / GLM | accepted | n/a | `low/medium/high` |

Effort is sent only when thinking is on: budget > 0, and the synthesizer additionally requires the planner's `is_complex`. Reasoning models that reject a non-default temperature *value* (DeepSeek V4 Pro, GLM 5.3) need the send-temperature switch off. A new Claude dialect may need a new `_ADAPTIVE_THINKING_MARKERS` substring.

### 5.5 Retiring a model

Add `retired → successor` to `RETIRED_MODELS` (`llm_models.py`). At `api` boot `remap_retired_models()` rewrites every `MODEL_SETTING_KEYS` value and every `llm_templates.model`, then busts the config caches. It is idempotent and best-effort. It leaves `llm_models` and `model_prices` alone.

Deploy order:
1. Edit the server `.env`.
2. `make rebuild`.
3. `make apply-llm-models`.
4. Disable the retired rows on the LLM-models page.

### 5.6 Agent editor overlay

A published agent-editor version can set a role's model and params (`x-target: runtime.*`). `apply_overlay` then drops that role's template, and the panels report `managed_roles`. Legacy-page writes that would *change* a managed value return 409 `managed_by_agent_editor`. A `@dev` variant runs its own dev deployment config. Details are in `CLAUDE.md`, section "Visual agent editor".

---

## 6. Provider key management

Implemented in `app/services/provider_secrets.py` and `admin_api/routes/llm_config.py`, under the keys section of **LLM & Search**.

- Keys are Fernet-encrypted in `provider_secrets` with `APP_SECRETS_KEY`. If that key is unset, editing is disabled and the panel shows env status read-only.
- `target='litellm'` covers `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY` and `GLM_API_KEY`. `export_litellm_env()` writes them to `LITELLM_SECRETS_FILE` (`/secrets/providers.env`), and the UI shows `pending_restart` until they are applied.
- **Apply:** `make apply-llm-keys` runs `docker compose up -d litellm`. A plain `restart` does **not** re-read `env_file`, and Compose ≥ 2.24 is required for `required: false`.
- `target='live'` covers the search keys (Tavily/Google), which `web_search.py` reads directly (`effective_search_key`).
- Backups (`scripts/backup.sh`) include `.env`, `providers.env`, `managed-models.yaml` and `config.yaml`. `.env` must travel with the DB dump, because `APP_SECRETS_KEY` decrypts `provider_secrets`. After a restore, run `make apply-llm-keys`.
- All services share **one master key** to the proxy. Per-service attribution comes from the app's `purpose` tags, not from LiteLLM keys.

---

## 7. Reliability: timeouts and retries

**Rule: a timed-out LLM request is never retried, at any layer.** The provider keeps generating after the client hangs up and bills the finished answer anyway. Before this rule, one slow analysis-B call reached OpenAI up to 15 times (3 client attempts × 5 loop attempts), and every copy was billed.

| Layer | Timeout | Retries on 429/5xx/connection | On timeout |
|---|---|---|---|
| `LLMClient` (shared) | `LLM_TIMEOUT_SECONDS` = 180 (or the template's `timeout_seconds`) | tenacity on `UpstreamError` | raises `UpstreamTimeoutError`, **not retried** |
| `chat_stream` open | same | one retry of the stream *open* (`_STREAM_OPEN_ATTEMPTS = 2`) | not retried. A mid-stream break is booked as `error_type='stream'` |
| Review loop (`improvement_loop/llm.py::call_parsed`) | `LOOP_LLM_TIMEOUT_SECONDS` = 600 (or the stage template's timeout) | 5 attempts, 2→30 s backoff. Also one retry with a doubled budget on truncation, and one JSON-only nudge on unparseable output | re-raised on the first attempt, counted as `errors_timeout` |
| LiteLLM proxy | `request_timeout: 600` (the ceiling) | `num_retries: 2` | `TimeoutErrorRetries: 0` |

Notes:
- Size timeouts for the **slowest legitimate** call. Raising them is free; lowering them costs money.
- Proxy retries and app retries multiply on 5xx/429. That is accepted, and timeouts are excluded at both layers.
- The outcome is booked **once per logical call**: `LLMClient.chat` wraps the tenacity-decorated `_chat_once`. Each retry increments `llm_retries_total` separately.
- **No fallbacks.** If a provider is down, every call to that model fails until an admin switches the role or template to another model. There is no automatic failover.

---

## 8. Token and cost accounting

### 8.1 Capture path

1. Each request or job opens a `usage_scope()`, which puts a per-turn `UsageAccumulator` in a ContextVar.
2. Each LLM call records `TokenUsage` with input, output and cached tokens plus `cost_usd`. It goes into:
   - `by_purpose`: drives the **user-facing** per-turn cost and tokens in the SSE `done` event, and quota accounting;
   - `by_purpose_model`: the admin breakdown, keyed by `(purpose, real model id)`;
   - `calls_by_purpose_model`: becomes `usage_events.call_count`.
3. `record_aux()` writes **only** to the admin breakdown. This is used for web search (estimated cost, no call count) and TTS (priced per character, counted as a call). These never inflate the user's per-turn total or their token quota.
4. At the end of a chat turn, `finalize_turn` (`app/api/turn_pipeline.py`) does the following. Readings book their own usage in `app/api/routes_readings.py` (same `usage_scope` + `record_usage_events`).
   - calls `record_usage_events()` (`app/services/audit.py`), which writes one `usage_events` row per `(purpose, model)` stamped with `profile_id`, `user_id`, `session_id`, `turn_id` and `site`;
   - calls `quota.record_token_usage(total)`;
   - calls `record_audit(...)`.

   Writes are fire-and-forget, so an accounting failure never breaks a turn.
5. In parallel, each call increments Prometheus `llm_tokens_total{kind}` and `llm_cost_usd_total` (§12).

Workers (improvement loop, newsletter, price refresh) record under a synthetic system principal with `site='internal'`. If no model-tagged usage was recorded, a legacy fallback writes per-purpose rows with model `(see litellm)`.

### 8.2 `usage_events` columns

| Column | Meaning |
|---|---|
| `profile_id`, `user_id`, `session_id`, `turn_id`, `site` | attribution. There is no FK on `user_id`, so history survives a user delete |
| `purpose`, `model` | the role tag (§1) and the real model id. Provider is derived via `provider_of()` |
| `input_tokens`, `output_tokens`, `cached_tokens` | from the normalised `usage`. `cached_tokens` = prompt-cache reads |
| `cost_usd` | stamped **at write time** from `model_prices`. Not retroactive: a later price edit does not change history |
| `call_count` | LLM calls folded into the row. NULL for web-search or legacy rows, counted as 1 |

### 8.3 Cost formula

`model_pricing.cost_for(model, input, output)` = `input/1e6 × input_per_million + output/1e6 × output_per_million`. Prices come from an in-process snapshot (300 s TTL) with this fallback chain: `model_prices` row → built-in `_SEED` → flat `_DEFAULT` (1.0 / 3.0 USD per 1M). Costs are therefore never zero, but an unpriced model is silently billed at the flat default. Check the Model pricing page after adding a model.

Special cases:
- **TTS** is priced per character through the same table: `cost_for(model, chars, 0)`.
- **Web search** uses `WEB_SEARCH_COST_USD_TAVILY` (0.008) and `WEB_SEARCH_COST_USD_GOOGLE` (0.005) per search. These are estimates, not billed figures.
- **Cached tokens are not discounted.** `model_prices.cached_input_per_million` is stored (and filled by the price job), but `cost_for` does not use it. Cache reads are therefore priced at the full input rate, and the estimate *overstates* the cost of prompt-cached Anthropic calls. See §15.

---

## 9. Pricing management

Implemented in `app/services/model_pricing.py`, `price_fetch.py`, `price_schedule_config.py`, `admin_api/routes/model_prices.py`, and the admin page **Agent → Model pricing**.

- **`model_prices`** stores USD per 1M tokens: `input_per_million`, `output_per_million`, and optional `cached_input_per_million`. Each row has a `source`:
  - `manual`: admin-owned, **never overwritten** by the job;
  - `websearch`;
  - `seed`: inserted at boot by `seed_prices()`.
- **`provider_price_sources`** holds one curated pricing-page URL per provider.
- **Daily refresh job.** The improvement-loop worker fires it under a daily guard (`prices.last_refresh_date`); the schedule lives in `prices.*` in `app_settings`. For each enabled provider, the job web-searches the pricing page (scoped to its domain), and an LLM extracts per-1M prices for the configured model ids. This covers Anthropic, OpenAI and GLM, which LiteLLM's price map could not. The job's own cost is recorded under the `price_fetch` purpose.
- **Admin API:**
  - `GET /model-prices`
  - `PUT` / `DELETE /model-prices` (model id in the body/query, never the path)
  - `PUT` / `DELETE /model-prices/sources`
  - `PATCH /model-prices/schedule`
  - `POST /model-prices/refresh`: runs now and returns `updated` / `skipped_manual` / `no_price`.
- Edits bust the in-process cache and apply to **new** calls only.
- **Stale naming:** the `ModelPrice` docstring still mentions `'litellm'` source rows, and the `POST /model-prices/refresh` docstring still says "LiteLLM price lookup". Both are legacy; LiteLLM's price map is not used.

---

## 10. Cost and token levers

| Lever | Setting | Effect / caveat |
|---|---|---|
| **Cheaper model per role** | per-role model or template (§5) | The biggest lever. Planner, classifier, suggestions and summary can use a fast or cheap model. `AGENT_PLANNER_MODEL` blank → primary |
| **Max output tokens per phase** | `AGENT_MAX_OUTPUT_TOKENS` (8196), `AGENT_PLANNER_MAX_TOKENS` (400), `AGENT_CLASSIFIER_MAX_TOKENS` (400), `AGENT_SUGGESTIONS_MAX_TOKENS` (300), loop `LOOP_*_MAX_TOKENS`, or template `max_output_tokens` | caps the worst case. Too low → truncation (`finish_reason=length`) |
| **Extended thinking** | `AGENT_THINKING_BUDGET_TOKENS` (8196, 0 = off), loop `LOOP_THINKING_BUDGET_TOKENS` (4096), template budget/effort | thinking tokens are billed as output. The synthesizer only thinks on `is_complex` turns |
| **Prompt caching** | `AGENT_PROMPT_CACHE_ENABLED` (false; runtime toggle) | Anthropic `cache_control` on the static planner/synth prefix. History is inserted *after* the system prompt so the breakpoint survives. Smoke-test cache hits before enabling. Savings don't show in `cost_usd` (§8.3); look at `cached_tokens` instead |
| **Semantic response cache** | `RESPONSE_CACHE_ENABLED` (true), threshold 0.90, TTL 24 h; admin **Agent → Response cache** | a hit skips plan, tools and synthesis entirely, so no LLM call is made. Only applies to first-turn questions without web search. Hit/miss stats in `respcache:stats:*` |
| **Topic-classifier cache** | `topic_classify:{sha256}` (24 h) | repeated questions skip the classifier call |
| **Verification** | `AGENT_VERIFICATION_ENABLED` (false), `…_COMPLEX_ONLY` | +1 call per turn (`synthesizer_draft` + `verifier`) |
| **Conversation memory** | `AGENT_HISTORY_RECENT_TURNS` (4), summariser every ~4 turns (`summarizer`) | bounds prompt growth on long chats. The summary model defaults to the classifier model |
| **Review-loop triage** | `LOOP_TRIAGE_MODE` (`model`/`all`/`feedback_only`), `LOOP_ANALYSIS_MODE` (`dual`/`single`), `LOOP_MAX_CONVERSATIONS` | triage is the loop's main cost lever. `dual` ≈ 2 analyses + a judge per turn |
| **Fix verification** | `LOOP_VERIFICATION_ENABLED` (false) | one full live agent turn per case |
| **Newsletter** | `max_items_per_issue`, `max_sources`, dedupe by `content_hash` | one classify call per *new* article |
| **Quotas** | §11 | cap per-user spend |

---

## 11. Quota management

Implemented in `app/guards/quota.py`, `app/services/quota_config.py`, `admin_api/routes/quota_config.py`, and the quota panel on the admin **Dashboard**.

| Type | Window | Redis key | Default |
|---|---|---|---|
| **Token quota** | fixed window: `hour/day/week/month` | `quota:daily_tokens:{user_id}` (name kept for continuity, whatever the period) | on, 100 000 / day |
| **Request quota** | fixed window: `hour/day/week/month` | `quota:requests:{user_id}` | off, 1 000 / day |
| **Per-minute throttle** | sliding 60 s (sorted set) | `quota:minute_requests:{user_id}` | 10 / min, always on, independent of the types |

- **Check before, charge after.** `check_and_reserve` runs in the guard chain *before* the SSE stream opens, so a 429 (`QuotaExceededError` / `RateLimitError`) propagates as an HTTP code. It reserves one request, check-then-INCR, which is deliberately not atomic. The turn's real token total is added at the end (`record_token_usage`). A user can therefore overshoot by up to one turn's tokens. Readings use the same guard. `check_throttle` touches only the burst window.
- **What counts:** the user-facing `by_purpose` total (§8.1). Web search and TTS (`record_aux`) don't count. Neither do worker or loop calls, which have no user quota.
- **Configuration layers:**
  1. env defaults: `AGENT_QUOTA_TOKEN_ENABLED`, `AGENT_PER_USER_DAILY_TOKEN_LIMIT`, `AGENT_QUOTA_TOKEN_PERIOD`, `AGENT_QUOTA_REQUEST_ENABLED`, `AGENT_PER_USER_REQUEST_LIMIT`, `AGENT_QUOTA_REQUEST_PERIOD`, `AGENT_PER_USER_PER_MINUTE_REQUESTS`;
  2. global runtime overrides in `app_settings` `__global__`, keys `quota.token_*` / `quota.request_*` (cache `config:quota_settings`);
  3. master toggle `quota_enforcement_enabled`;
  4. per-user overrides on the `users` row (`quota_disabled`, `daily_token_limit`, `request_limit`). Periods stay global.

  `AGENT_QUOTA_DISABLED=true` bypasses everything (dev/test).
- **Scope:** quotas are **per user, across all profiles**. Usage is still reported per profile.
- **Admin operations:**
  - `GET/PATCH /api/v1/quota-config`
  - `GET /api/v1/redis/quota[/{user_id}]`
  - `DELETE /api/v1/redis/quota/{user_id}`: reset; clears the token and request counters with a single DELETE each.
  - Users see their own state at `GET /api/v1/quota`, shown by the `DailyUsageCard`.
- **Monitoring:** rejections increment `guard_rejections_total{reason="quota"|"rate_limit"}`, and the `GuardRejectionSpike` alert fires above 1/s.

---

## 12. Metrics, monitoring and alerting

### 12.1 Prometheus series (`app/core/metrics.py`)

All series are exposed on `/metrics` of `api` and `admin-api`. There are **no user labels**, so cardinality is bounded to sites × profiles × catalogue.

| Series | Labels | Meaning |
|---|---|---|
| `llm_requests_total` | `service, site, profile, model, purpose, outcome` | logical calls by final outcome `ok`/`error`/`timeout`, counted after retries |
| `llm_retries_total` | `service, model, purpose` | transient failures that were retried |
| `llm_tokens_total` | `service, site, profile, model, kind` | tokens by `input`/`output`/`cached` |
| `llm_cost_usd_total` | `service, site, profile, model` | estimated USD (LLM + web search) |
| `agent_events_total` | `service, site, profile, event_type` | `request_completed`, `reading_completed`, `rejected_*` … |
| `guard_rejections_total` | `service, reason` | auth/profile/language/quota/rate_limit/illegal/off_topic |
| `http_requests_total`, `http_request_duration_seconds`, `http_requests_in_progress` | `service, method, path[, status]` | HTTP layer |

⚠️ **Worker processes are not scraped.** Scheduled worker jobs (Review loop, newsletter, nightly price refresh) never appear in Prometheus. A manual `POST /model-prices/refresh` and the editor assistant run inside admin-api and do appear. **Postgres (`usage_events`, `llm_call_errors`) is authoritative** for totals. Prometheus is for live rates and alerting on user-facing traffic.

**LiteLLM's own metrics are off.** The scrape job in `monitoring/prometheus/prometheus.yml.tpl` is commented out: `/metrics` is auth-gated and requires the `prometheus` callback, and the unauthenticated scrape only logged "Malformed API Key". To re-enable it, add `litellm_settings.callbacks: ["prometheus"]` and restore the job with `authorization: { credentials: <master key> }`, rendered through `entrypoint.sh`.

### 12.2 Grafana

Grafana runs from the optional `monitoring/` module. It is proxied at `/monitoring` and embedded in the admin console under **Monitoring → Metrics**. Access is gated to admins via `forward_auth`.

| Dashboard | LLM-relevant panels |
|---|---|
| **pacod — Usage** (`pacod-usage.json`) | LLM calls (24h); calls/s by site, profile & model; error ratio by model (15m); failed calls per 15m by outcome & model; retries/s by model & purpose; tokens/s by kind and by model; estimated cost $/h by site & profile; estimated cost (24h); agent events and rejected requests |
| `pacod-app.json` | HTTP latency/5xx, guard rejections |
| `pacod-infra.json`, `pacod-web.json`, `pacod-crawlers.json` | infra, edge traffic, crawlers (not LLM) |

### 12.3 Alert rules (`monitoring/prometheus/alerts/app.rules.yml`, group `pacod-llm`)

| Alert | Condition | Severity |
|---|---|---|
| `LlmErrorRateHigh` | > 10 % non-ok calls per (site, model) over 15 m, gated on > 10 calls, `for: 10m` | warning |
| `LlmTimeoutsSpike` | > 5 timeouts in 15 m | info |
| `LlmAllCallsFailing` | zero ok calls while > 5 attempted in 15 m (`or vector(0)` so it fires even if nothing ever succeeded) | critical |
| `GuardRejectionSpike` | any guard reason > 1/s for 10 m (includes quota/rate_limit) | info |

Alertmanager routing is in `monitoring/alertmanager/alertmanager.yml`. There is **no spend alert** yet; see §15.

### 12.4 Logs

Every service logs through structlog (`get_logger(__name__)`, context as kwargs). LLM-relevant events include `loop_llm_timeout`, `loop_json_truncated`, `loop_json_unparseable_retrying`, `model_pricing_refresh_failed`, the `redis_request`/`postgres_request` pairs, and LiteLLM's own container logs (`make logs`). The `LLMClient` logger gets a copy of the request kwargs with image payloads redacted (`_redact_content` → `<image ~42kb>`).

---

## 13. Analytics, statistics and reporting

| Surface | Backed by | Answers |
|---|---|---|
| **LLM & Search → Usage & costs** | `GET /api/v1/usage-events/overview` (cost/tokens for the current hour/day/week/month), `GET /usage-events/summary?bucket=hour\|day\|week\|month&range_start&range_end&provider&model` (time series + `model_series` stacked per model + `by_provider`/`by_model`/`by_purpose`/`by_user`) | "what did we spend, on which model/provider/purpose/user, over time?" Cost/tokens y-axis toggle |
| **Monitoring → Analytics** | `admin_api/routes/analytics.py` + `app/services/analytics_queries.py`: `GET /analytics/overview` (totals, `errors_by_type`, series, `by_site`, `by_site_profile`, `by_model`), `GET /analytics/users[/{id}]` | requests **and** LLM usage/errors per site (`pacod`/`vanga`/`vanga-app`/`openwebui`/`internal`), per profile, per user; drill into one user (IPs, activity log) |
| **Conversations** → turn detail | `GET /api/v1/usage-events/turn/{turn_id}` | per-(purpose, model) tokens and cost of one turn |
| **Chat UI** (end user) | SSE `done` event + `GET /chat/sessions/{id}/turns` (`usage`) | the user's own per-turn tokens and cost (user-facing purposes only) |
| **Quota** | `GET /api/v1/quota`, admin `/redis/quota` | per-user consumption against limits |
| **Response cache page** | `GET /response-cache-config` (`stats`) | hit/miss counts, i.e. LLM calls avoided |
| **Review process** | `loop_runs.report_md` + `stats` (`errors_truncated`/`_unparseable`/`_upstream`/`_timeout`, `llm_retries`, `models_used`, `mode_effective`), `GET /loop-findings/stats` | loop cost and LLM health per run; quality metrics (dismissal rate by failure mode) |
| **Model pricing page** | `model_prices`, `RefreshResult` | current rates, last refresh outcome |
| **Grafana pacod — Usage** | Prometheus | live rates (HTTP services only) |

Filtering:
- The admin `profile_id` selector scopes agent/LLM metrics. Web/page rows have no profile.
- The `site` axis comes from the trusted `X-Pacod-Site` header (`ANALYTICS_TRUSTED_SITES`). It is never used for authorization.

Retention:
- `usage_events` and `debug_interactions` are **never pruned**. The worker prunes only turn evidence, crawler visits, analytics tables and user activity.
- `llm_call_errors`, `web_requests` and `page_views` are pruned by the worker after `ANALYTICS_RETENTION_DAYS` (180).
- `debug_interactions` holds full prompts, so treat it as sensitive.

**Ad-hoc SQL** (Adminer on `:8081`, `tools` profile):

```sql
-- spend by model, last 30 days
SELECT model, SUM(cost_usd) usd, SUM(input_tokens) inp, SUM(output_tokens) outp,
       SUM(cached_tokens) cached, SUM(COALESCE(call_count,1)) calls
FROM usage_events WHERE occurred_at > now() - interval '30 days'
GROUP BY model ORDER BY usd DESC;

-- cost per purpose per profile, this month
SELECT profile_id, purpose, ROUND(SUM(cost_usd)::numeric, 4) usd
FROM usage_events WHERE occurred_at >= date_trunc('month', now())
GROUP BY 1, 2 ORDER BY usd DESC;

-- failures by model and type, last 24 h
SELECT model, purpose, error_type, COUNT(*), MAX(occurred_at)
FROM llm_call_errors WHERE occurred_at > now() - interval '1 day'
GROUP BY 1, 2, 3 ORDER BY 4 DESC;
```

---

## 14. Debugging LLM behaviour

| Tool | What it shows | How to turn it on / reach it |
|---|---|---|
| **Debug Console** (`debug_interactions`) | every LLM/RAG/tool/decision step of a turn, **untruncated** request + response, `duration_ms`, sequence: planner, tool calls, synthesizer, verifier, `conversation_memory` decision, `summarizer`, `reading_vision`, `reading_tts`, loop analyses | `AGENT_DEBUG_AUDIT_ENABLED` or the runtime toggle at admin `/debug-config` (`GET/PATCH /api/v1/debug-config`). UI **Debug Console** (`GET /debug-interactions/turns`, `/{id}`). Expensive, so off by default. Image payloads are redacted. `debug_scope(...)` must get `profile_id` or rows are hidden in profile scope |
| **Turn evidence** (`turn_evidence`) | exactly what the synthesizer read: context verbatim, directives in force, tool calls, compacted history | on by default (`TURN_EVIDENCE_ENABLED`). Same `/debug-config` panel. Used by the Review loop |
| **LLM error log** (`llm_call_errors`) | one row per **final** failure: model, purpose, `error_type` (`timeout`/`upstream`/`stream`), status code, message, attempts, site/profile/user/turn | always on (`app/services/llm_errors.py`). Surfaced in Analytics `errors_by_type` |
| **Per-turn usage** | tokens and cost per (purpose, model) | Conversations detail dialog |
| **Audit log** (`audit_log`) | question, answer, classification, tools used, `flow_steps`, citations, `duration_ms`, `agent_variant` | Conversations view |
| **Retrieval traces** | KB candidates used and discarded per search | `/debug-console/{turn_id}` |
| **Conversation memory** | the summary + verbatim turns fed to a turn | Debug Console decision row; Redis admin `GET/DELETE /api/v1/redis/conversation-summaries` |
| **Single-turn review** | runs one suspicious answer through the whole Review pipeline (evidence, analysis, remediation) | Conversations → review; `POST /loop-config/run {turn_id}` |
| **Proxy transformation** | what LiteLLM actually sends a provider for given params | `POST /utils/transform_request` on the proxy, called from inside `agent-net` with the master key |
| **Smoke test** | one real SSE chat turn end to end | `make smoke-local` (local), or `exec -T api curl …:8000` with a dev token (local only) |

Symptom → first check:

| Symptom | Check |
|---|---|
| Every call fails | `LlmAllCallsFailing`; `make logs` on litellm; keys applied (`make apply-llm-keys`)? `managed-models.yaml` present? Provider status/quota |
| "Model not found" / 400 after adding a model | catalogue row `in_gateway=false` → `make apply-llm-models`; key env var wired in `provider_secrets._KEYS` |
| Anthropic 400 "temperature is deprecated" | model missing from the adaptive markers / capabilities (§5.4) |
| Reasoning model 400 on temperature | turn the send-temperature switch off, or use a template with `temperature` NULL |
| Gemini 400 on history | message normalisation (`conversation_memory._normalize`) |
| Truncated JSON / empty loop findings | `finish_reason=length` → raise the stage's max tokens; loop stats `errors_truncated` |
| Timeouts | `LlmTimeoutsSpike`; slow provider vs a too-small `LLM_TIMEOUT_SECONDS`/template timeout. Never add retries |
| Cost looks wrong | unpriced model → flat default (§8.3); cached tokens not discounted; prices aren't retroactive |
| Cost missing in Grafana but present in admin | worker calls aren't scraped (§12.1) |
| User gets 429 | quota vs throttle (`detail`), `/redis/quota/{user}`, per-user override, period |
| Template change not applied | managed by the agent editor (`managed_roles`)? Cache TTL 60 s |
| Prompt cache not hitting | `AGENT_PROMPT_CACHE_ENABLED`, Anthropic model, `cached_tokens` in usage rows |

---

## 15. Risks and open items

- **Unpinned LiteLLM image (`main-latest`).** Behaviour the app depends on can change on any pull: `provider_specific_fields.web_search_results`, `delta.thinking` / `reasoning_content`, the `reasoning_effort` mapping, `drop_params` semantics, `include:` handling. Pin a tested tag.
- **Implicit reliance on `drop_params: true`.** Turning it off would break every non-Anthropic call that receives `cache_control`, and possibly the `thinking` payloads.
- **Double retry layer.** Proxy `num_retries: 2` × app tenacity can multiply attempts on 5xx/429. Timeouts are excluded at both layers.
- **No proxy health check.** Dependents use `condition: service_started`, so a proxy that fails to boot (e.g. a missing include file) only shows up at the first LLM call.
- **No LiteLLM metrics, and workers aren't scraped.** Prometheus undercounts. Use Postgres for totals.
- **No spend alerting or budget cap.** Quotas cap tokens per user, but there is no global daily-spend limit or alert. Worker/loop spend is uncapped apart from config bounds. A Prometheus rule on `increase(llm_cost_usd_total[1h])`, or a worker check on `usage_events`, would close the gap.
- **Cached-input price unused.** `cached_input_per_million` is stored but `cost_for` ignores it, so prompt-cached calls are over-estimated.
- **Flat default price.** Unpriced models are silently costed at 1.0/3.0 USD per 1M.
- **No automatic failover.** A provider outage takes down every role on that provider until an admin repoints it.
- **Single master key shared by all services.** No per-service attribution at the proxy; the app's `purpose` tags cover it.
- **Stale naming.** `ModelPrice` and refresh-route docstrings still mention LiteLLM pricing; `usage_events` legacy rows say `(see litellm)`.
- **Vision via LiteLLM.** Readings rely on LiteLLM translating image blocks for the selected `vision_model`. Verify this when switching providers.

---

## 16. File index

| Area | Files |
|---|---|
| Proxy config | `docker/litellm/config.yaml`, `docker/litellm/secrets/{managed-models.yaml,providers.env}` |
| Compose / ops | `docker-compose.yml` (`litellm` service + `LITELLM_*` env), `Makefile` (`apply-llm-keys`, `apply-llm-models`, `ensure-llm-models`), `../scripts/backup.sh`, `../scripts/restore.sh` |
| Settings | `app/core/config.py` (`litellm_*`, `agent_*`, `loop_*`, `llm_timeout_seconds`, `agent_quota_*`, `web_search_cost_*`), `.env.example` |
| Client | `app/services/llm_client.py` (chat, stream, dialects, prompt caching, `UsageAccumulator`, cost), `llm_capabilities.py`, `llm_params.py` |
| Models / templates | `app/services/llm_models.py`, `llm_templates.py`, `llm_templates_migration.py`, `admin_api/routes/{llm_models,llm_templates}.py` |
| Runtime config | `app/services/agent_runtime_config.py`, `runtime_settings.py`, `loop_runtime_config.py`, `newsletter_runtime_config.py`, `app/readings/config.py`, `app/agent_editor/{effective,assistant_config}.py`, `admin_api/routes/llm_config.py` |
| Keys | `app/services/provider_secrets.py`, `admin_api/routes/llm_config.py` |
| Accounting | `app/services/audit.py` (`record_usage_events`), `app/api/turn_pipeline.py` (`finalize_turn`), `app/services/llm_errors.py`, `app/core/db_models.py` (`UsageEvent`, `LlmCallError`, `ModelPrice`, `LlmModel`, `LlmTemplate`) |
| Pricing | `app/services/model_pricing.py`, `price_fetch.py`, `price_schedule_config.py`, `admin_api/routes/model_prices.py` |
| Quotas | `app/guards/quota.py`, `app/services/quota_config.py`, `admin_api/routes/{quota_config,redis_store}.py` |
| Metrics | `app/core/metrics.py`, `app/core/request_context.py` (`current_site`) |
| Analytics | `admin_api/routes/{usage_events,analytics}.py`, `app/services/analytics_queries.py` |
| Debugging | `app/services/debug_audit.py`, `admin_api/routes/{debug_config,debug_interactions}.py`, `app/services/turn_evidence_config.py` |
| Other callers | `app/readings/{interpret,tts}.py`, `app/services/{web_search_native,newsletter_classify,conversation_memory}.py`, `improvement_loop/llm.py`, `newsletter_worker/`, `app/agent_editor/assistant.py` |
| Monitoring | `../monitoring/prometheus/prometheus.yml.tpl` (disabled LiteLLM job), `../monitoring/prometheus/alerts/app.rules.yml`, `../monitoring/grafana/dashboards/pacod-usage.json`, `../monitoring/alertmanager/alertmanager.yml` |
| Frontend (`pacod-ui/frontend/src/routes/(app)/`) | `llm-config/` (config, templates, keys, usage & costs), `llm-models/`, `model-prices/`, `analytics/`, `dashboard/` (quota panel), `debug-console/`, `response-cache/`, `review-process/`, `metrics/` (Grafana embed), `redis/` |
