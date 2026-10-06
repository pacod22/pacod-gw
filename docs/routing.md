# Routing among the models of a template

A template can hold several models. A routing scheme decides, per call, which of them serves first and which take over when it fails. The design is section 7.5 of the [concept](ai-gateway-concept.md). This page shows how to set it up.

## 1. Give the template a pool

Replace `model` with `models`. Each entry has a `tier` (a label schemes choose by), a `weight` (share of calls for the weighted algorithms; `0` for a backup only) and a `priority` (lower first).

```json
PUT /api/v1/orgs/{org}/workspaces/{ws}/templates/{id}
{
  "name": "support-bot",
  "spec": {
    "models": [
      { "model": "anthropic/claude-haiku-4-5", "tier": "small", "weight": 3 },
      { "model": "openai/gpt-5-mini", "tier": "small", "weight": 1 },
      { "model": "anthropic/claude-sonnet-5-5", "tier": "large" },
      { "model": "openai/gpt-5", "tier": "large", "priority": 1 }
    ],
    "max_output_tokens": 2000
  }
}
```

Without a scheme the pool is used by priority, up to three models: the first serves, and the next one takes over on a 429, a 5xx or a lost connection. That alone is main and backups.

In the console: the template's page, **Models → Several models, chosen per call**.

## 2. Write a scheme

Schemes belong to the organisation (**Organisation → Routing schemes** in the console).

```json
POST /api/v1/orgs/{org}/routing-schemes
{
  "name": "by-complexity",
  "description": "Cheap models for easy calls",
  "rules": [
    { "name": "easy",
      "when": { "complexity": ["low"] },
      "then": { "tiers": ["small"], "algorithm": "weighted", "overflow": "rest_of_pool" } },
    { "name": "review",
      "when": { "task": { "in": ["code-review"] }, "agent_id": { "prefix": ["ci-"] } },
      "then": { "tiers": ["large"], "algorithm": "priority" } },
    { "name": "long-input",
      "when": { "input_tokens": { "at_least": 100000 } },
      "then": { "tiers": ["large"], "algorithm": "cheapest" } }
  ],
  "default": { "tiers": ["large", "small"], "algorithm": "round_robin" },
  "failover": { "max_targets": 3 }
}
```

Callers state what routing looks at with headers or in the body's `gateway` object:

| Header | `gateway` field |
|---|---|
| `x-gw-complexity: low` | `"complexity": "low"` |
| `x-gw-agent-id: ci-7` | `"agent_id": "ci-7"` |
| `x-gw-task: code-review` | `"task": "code-review"` |
| `x-gw-user-id: u-12` | `"user_id": "u-12"` |
| `x-gw-tags: team=search` | `"tags": { "team": "search" }` |
| `x-gw-session: s-99` | `"session_id": "s-99"` (for session affinity) |

## 3. Assign it

The most specific assignment applies:

- **Template**: `spec.routing_scheme`.
- **Workspace**: `settings.routing_scheme`.
- **Organisation**: set by the organisation's administrators (and platform administrators), on the organisation's **Routing schemes** page or with `PUT /api/v1/orgs/{org}/routing-assignment` and a body of `{ "scheme": "by-complexity", "locked": true }`. While it is locked, the organisation's scheme applies to every workspace and template; their own assignments are kept but ignored. Anyone who can see the organisation can read it with `GET`, which also lists the workspaces and templates that name a scheme of their own.

## 4. Check it

- `POST /api/v1/orgs/{org}/workspaces/{ws}/templates/{id}/route` with `{ "complexity": "low" }` shows the matching rule and the models in order, with their breakers. The console has it on the template's page as **Try the routing**.
- `GET …/templates/{id}/check` warns about tiers the scheme names that the pool lacks.
- Every response names the route: `x-gw-route: by-complexity/easy`, `x-gw-model-served`, and `x-gw-failovers` when the call moved on.
- Reports filter and group by `routing_scheme` and `routing_rule`; events also carry `failovers` and `models_tried`.
- `GET /api/v1/monitoring/routing` and the metric `gw_breaker_open` show models set aside; `gw_route_failovers_total` counts failovers.

## Recipes

| Goal | Scheme |
|---|---|
| Main and backups | No scheme, or `"default": { "algorithm": "priority" }`. Priorities in the pool. |
| Spread load | `"algorithm": "weighted"` or `"weighted_round_robin"` with weights in the pool. |
| Canary | A weighted rule; the new model with weight 1 against 19. |
| Keep prompt caches warm | `"affinity": "session"`; callers send `x-gw-session`. |
| Cost tiers | Rules on `complexity` or `task` choose `small` or `large`; `"overflow": "rest_of_pool"` keeps calls alive when a tier is down. |
| Fastest model | `"algorithm": "lowest_latency"`. Measured per instance from managed calls. |
| Long inputs | `"input_tokens": { "at_least": … }`. Models whose context window is too small are passed over anyway. |
| Something the typed conditions cannot say | `"cel": "task.startsWith(\"report-\") && !stream"`. |

## Failover, briefly

- **When it moves on.** A call moves on before the first byte of the answer, on 429, 5xx, a lost connection, or a gateway limit that counts only that model. Timeouts move on only with `"failover": { "on": { "timeout": true } }`.
- **Attempts.** Each model gets one attempt in a pool (`attempts_per_target`). `x-gw-retries` caps the attempts of the whole call.
- **Time.** `per_target_timeout_ms` caps the time each model gets. `deadline_ms` stops trying further models.
- **Dialects.** A model of another dialect is reached through translation, so an OpenAI-shaped call can fail over to Anthropic and back.
- **Breaker.** After five failures in a row a model goes last for 30 s, doubling up to 5 min. Set the thresholds in `GlobalSettings.breaker`. This state is per instance.
