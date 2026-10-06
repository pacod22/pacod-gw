# Testing a running gateway

The tester runs test cases against a running pacod-gw installation. It uses only the gateway's public interfaces, the same ones applications and the console use. Platform admins start runs from the console under **Platform → Testing**, choose the cases, follow the run live, and read every step afterwards.

It complements the other checks:
- `make verify` runs the Rust and console tests against the code.
- `make smoke-mock` and `make smoke-live` run the fixed smoke flow from a development machine.
- The tester runs inside a deployment, production included. Its cases are YAML files that can be added without rebuilding anything.

## 1. How it fits together

```text
browser ──► console (server side) ──Bearer <admin session>──► tester :8095
                                                              ├─► core :8081  /api/v1   (same session)
                                                              ├─► core :8080  data plane (scratch key)
                                                              └─► mock provider          (mock mode only)
```

- **The tester is its own container** (`deploy/docker/tester.Dockerfile`, compose service `tester`, Helm deployment `<release>-tester`). It runs Python 3.13 and keeps its run history and uploaded suites on a volume mounted at `/data`. It sits on the internal network only. The console is its one client, and Caddy and the Ingress do not forward to it.
- **Only platform admins can use it.** This is enforced in three places:
  1. The Testing item in the console's navigation appears only for platform admins.
  2. Every page, form action and event stream under `/platform/testing` refuses anyone else with 403.
  3. The tester trusts no caller because of where it sits on the network. For every request it asks the core (`GET /api/v1/session`) whose session it was given, and it serves platform admins only.
- **It has no secret of its own.** A run uses the session of the admin who started it, so everything the run does appears in the audit log under that admin. The session token is kept in memory for the length of the run and never stored. If the admin signs out during a run, the remaining management calls fail.
- **One run goes at a time.** A second start while one is going is refused with "A run is going".

## 2. Modes

| | Mock | Live |
|---|---|---|
| Who answers | A scripted mock provider (`gw-mock`) | Models that are already set up, chosen on the Run page |
| Available | When the tester has `TESTER_MOCK_URL`. The development stack sets it; production usually does not. | Always |
| Cost | None | One or two small calls per case, at the provider's prices |
| What it can check | Everything, including failover, which needs the mock to fail on purpose | Everything except the mock-only cases |

In mock mode the tester sets up two providers, `tester-mock-openai` and `tester-mock-anthropic`, with one model each, `tester-mock/gpt-mock` and `tester-mock/claude-mock`. It does this once and reuses them on later runs. The gateway only deletes models that were never active, and a provider only when no model uses it, so new ones on every run would pile up.

In live mode you choose an active model of an OpenAI-dialect provider, of an Anthropic provider, or of both. The tester checks the state and family of each and grants it to the run's scratch organisations. It never handles provider keys. A case that needs a model you did not choose is skipped, and the Run page says so before you start.

## 3. What a run does

For each suite with `fixture: scratch` (the default), the tester does three things in order:

1. **Setup.** It creates the organisation `test-<run>-<suite>`, with a workspace (audit mode `on_request`) and an API key. It grants the mode's models to that organisation, then runs the suite's own `setup` steps, for example creating templates.
2. **Cases.** It runs each case's steps in order. A case stops at its first failed step.
3. **Clean-up.** It deletes everything the suite created, newest first, which ends with the organisation. Clean-up also runs after failures and after a cancel. Whatever cannot be deleted is listed as *left over* on the run's page and in the history.

Suites with `fixture: none` create nothing; the health checks are an example.

**Keep what the run creates** skips the clean-up, so you can inspect the objects. Remove them afterwards; the run's page lists their paths.

**Secrets never reach the stored runs.** The session token, the scratch keys and the mock credential are replaced by `[redacted]` in every recorded request, response and message. Gateway keys and tokens are also caught by their shape. If an answer of the gateway contains one of the run's secrets, that step fails.

## 4. The built-in catalog

| Suite | Cases |
|---|---|
| `health` | Data plane `/healthz` and `/readyz`; management `/readyz`; the OpenAPI document is served; the session is a platform admin's |
| `auth` | Calls without a key and with an unknown key get 401; a revoked key stops working within 2 s; the management API refuses calls without a session or with an unknown token |
| `proxy` | OpenAI dialect relayed unchanged; Anthropic stream through a template |
| `managed` | Chat completion with cost; stream with cost on the usage chunk; Anthropic interface; translation each way; native streams from both families; the model list shows models and templates |
| `limits` | A daily limit of one request: the second call gets 402, and the quota endpoint shows the use |
| `routing` | A rule picks the tier; without a matching rule the large tier serves; a 503 fails over to the small model (mock only) |
| `observability` | The request log has the call with timings and cost; an audit record shows the bodies and what the gateway changed |

The suites are in `tester/src/pgw_tester/cases/`. On the **Cases** tab, each suite's name links to its file, so any of them can serve as an example for your own.

## 5. Writing a suite

A suite is a YAML file. Upload it on the **Cases** tab. It is validated first, and every problem is reported. An uploaded suite with the same name as one uploaded earlier replaces it. The names of built-in suites and the ids of all cases are reserved.

```yaml
suite: my-checks                 # a slug: a-z, 0-9 and -, unique
title: My checks
description: What the suite is for.
fixture: scratch                 # or none
setup:                           # optional steps before the cases; their variables are kept
  - name: create the template writer
    needs: [openai]
    mgmt:
      method: POST
      path: /orgs/{{org}}/workspaces/{{ws}}/templates
      body: { name: writer, spec: { model: "{{model.openai}}" } }
    expect: { status: 201 }
cases:
  - id: my-writer-call           # unique over the whole catalog
    title: "the writer template answers with a cost"
    modes: [mock, live]          # default: both
    needs: [openai]              # openai, anthropic, mock_control
    steps:
      - call:
          method: POST
          path: /v1/chat/completions
          body:
            model: "@writer"
            messages: [{ role: user, content: "Reply with the single word: pong" }]
        expect:
          headers:
            x-gw-cost: { exists: true }
          json:
            usage.cost.amount: { type: number, gt: 0 }
        save: { request_id: header.x-gw-request-id }
```

### Steps

Each step has exactly one of these:

| Key | Sends to | Authenticates with (default) |
|---|---|---|
| `mgmt` | The management API, below `/api/v1`. With `base: root`, the path is used as given, for example `/readyz`. | `session`: the admin's session |
| `call` | The data plane | `bearer`: the key in `key`, which defaults to `{{key}}` (the scratch key) |
| `mock` | The mock provider's control API, for example `POST /__mock/script` | `none` |
| `sleep` | Nothing; it waits, for example `sleep: 500ms` | – |

A request has `method` (default `GET`), `path`, `query`, `headers`, `body` and `timeout` (default 60 s). It may also set `auth`, which is `session`, `bearer`, `x-api-key` or `none`, and `key`. A body that is not a string is sent as JSON.

Further keys of a step:
- `name` labels the step in the results.
- `expect` holds the checks described below. Without `status`, any 2xx passes.
- `save` keeps a value for later steps, as `var: json.<path>`, `var: header.<name>` or `var: status`. A value saved from a field called `secret`, `token`, `key` or `password` is redacted from then on.
- `retry` repeats the step until its checks pass, for example `retry: { within: 2s, every: 100ms }`. It suits things that arrive with a delay, such as request-log entries and audit records.
- `needs` skips the step when a model or the mock is missing.
- `cleanup: false` leaves an object a POST created when the suite is cleaned up.

### Variables

Write `{{name}}` in any value, including inside checks. A value that is a single variable keeps its type, so a number stays a number. Every suite has `run_id`, `suffix` and `mode`. A scratch suite also has:

| Variable | Value |
|---|---|
| `org`, `ws` | The ids of the scratch organisation and workspace |
| `key`, `key_id` | The scratch API key and its id |
| `model.openai`, `model.anthropic` | The model names, as calls use them |
| `model_id.openai`, `model_id.anthropic` | The ids in the model repository |
| `provider.openai`, `provider.anthropic` | Provider names, as in `/proxy/{provider}/…` |
| `upstream.openai`, `upstream.anthropic` | The provider's own model ids |

Variables saved in `setup` are visible to every case. Variables a case saves stay inside that case.

### Checks

| Under `expect` | Checks |
|---|---|
| `status` | A status, or a list of accepted ones |
| `headers` | `{name: check}`. Header values are text, so `"1"` and `1` compare equal. |
| `json` | `{path: check}`. A path such as `choices.0.message.content`; `-1` is the last element; `*` maps over a list, as in `data.*.id`. |
| `body` | One check on the response text |
| `sse_last` | `{path: check}` on the JSON of the last `data:` line of an event stream |
| `sse_events` | Event names that must appear, such as `[message_stop]` |

A check is a value, meaning *equals*, or a mapping of one or more of these operators:
- `equals`, `not_equals`, `one_of`
- `exists` (with `present` as an alias)
- `contains`, `contains_all`, `not_contains`
- `matches` (a regular expression), `starts_with`, `ends_with`
- `type` (`string`, `number`, `integer`, `boolean`, `object`, `array`, `null`)
- `gt`, `gte`, `lt`, `lte`
- `length`, which takes a value or a further check

`contains` works on text, on lists (as membership) and on objects (as a key).

## 6. Configuration

| Variable | Where | Meaning |
|---|---|---|
| `TESTER_URL` | console | The tester's address, for example `http://tester:8095`. Without it, the Testing page says testing is not set up. |
| `PGW_CORE_ADMIN_URL` | tester | The management listener, for example `http://core:8081` |
| `PGW_CORE_DATA_URL` | tester | The data plane, for example `http://core:8080` |
| `TESTER_MOCK_URL` | tester | The mock provider as the gateway reaches it. Empty: no mock mode. |
| `TESTER_MOCK_CONTROL_URL` | tester | The mock as the tester reaches it. Defaults to `TESTER_MOCK_URL`. |
| `TESTER_DATA_DIR` | tester | Runs and uploaded suites (default `/data`) |
| `TESTER_KEEP_RUNS` | tester | How many runs the history keeps (default 100) |
| `TESTER_LISTEN` | tester | Listen address (default `0.0.0.0:8095`) |

With Helm, the values under `tester:` cover the same settings. `tester.mockUrl` turns on mock mode, and `tester.persistence` keeps the history on a PVC.

## 7. Working on the tester

```sh
make tester-test      # ruff and pytest in tester/ (needs uv)
make run-tester       # the tester on the host, against scripts/dev-core.sh, on 127.0.0.1:28095
```

In the development stack (`make dev-up`), the tester is published on `127.0.0.1:18095` with mock mode on. The tests fake the core with `httpx.MockTransport` (`tester/tests/fakecore.py`), so they need no running services.
