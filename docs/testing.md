# Testing a running gateway

The tester runs test cases against a running pacod-gw installation, sends single messages that you compose by hand and trace through the gateway (section 6), and runs load tests with load agents in containers of their own (section 9). It uses only the gateway's public interfaces, the same ones applications and the console use. Platform admins start runs from the console under **Platform → Testing**, choose the cases, follow the run live, and read every step afterwards.

It complements the other checks:
- `make verify` runs the Rust and console tests against the code.
- `make smoke-mock` and `make smoke-live` run the fixed smoke flow from a development machine.
- The tester runs inside a deployment, production included. Its cases are YAML files that can be added without rebuilding anything.

The minimal gateway ([lite.md](lite.md)) has no console and no tester; it is tested with
`cargo test -p pacod-gw-lite`, `make lite-smoke` and `make lite-live` (lite.md, section 11).

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
- **A run acts as the admin who started it.** Everything the run does appears in the audit log under that admin. The session token is kept in memory for the length of the run and never stored. With pacod-id, the run takes a delegated grant of that session with the tester's own client secret (`TESTER_ID_CLIENT_SECRET`), because the console's token ends after five minutes. If the admin signs out during a run, the run stops. With load tests, the tester also holds the token it shares with the load agents (`TESTER_LOAD_TOKEN`).
- **One test goes at a time.** A run of cases and a load test exclude each other: a load test changes what the cases measure (limits, latency, the circuit breaker), and the cases' calls would show in the load test's figures. A second start is refused with "A test is going". Messages are single calls and are not held back.

## 2. Modes

| | Mock | Live |
|---|---|---|
| Who answers | A scripted mock provider (`gw-mock`) | Models that are already set up, chosen on the Run page |
| Available | When the tester has `TESTER_MOCK_URL`. The development stack sets it; production usually does not. | Always |
| Cost | None | One or two small calls per case, at the provider's prices |
| What it can check | Everything, including failover, which needs the mock to fail on purpose | Everything except the mock-only cases |

In mock mode the tester sets up three providers, `tester-mock-openai`, `tester-mock-anthropic` and `tester-mock-gemini` (the mock's emulation of Gemini's own API), with one model each, `tester-mock/gpt-mock`, `tester-mock/claude-mock` and `tester-mock/gemini-mock`. For the mock-only cases it also sets up an embedding model (`tester-mock/embed-mock` on `tester-mock-openai`), xAI (`tester-mock-xai`, `tester-mock/grok-mock`) and Vertex AI's express mode (`tester-mock-vertex`, `tester-mock/vertex-mock`), the last two from their provider presets. It does this once and reuses them on later runs, finding them by name. The gateway only deletes models that were never active, and a provider only when no model uses it, so new ones on every run would pile up.

In live mode you choose up to three active models, one per slot: an **OpenAI-dialect** model (any provider managed mode reaches in OpenAI's dialect: OpenAI, DeepSeek, GLM, Kimi, Ollama, or Gemini's compatible endpoint), an **Anthropic** model, and a model on **Gemini's own API** (a Gemini provider with `managed_api: native`). The tester checks the state of each and that its provider serves the slot, and grants it to the run's scratch organisations. It never handles provider keys. A case that needs a model you did not choose is skipped, and the Run page says so before you start.

## 3. What a run does

For each suite with `fixture: scratch` (the default), the tester does three things in order:

1. **Setup.** It creates the organisation `test-<run>-<suite>`, with a workspace (audit mode `on_request`) and an API key. It grants the mode's models to that organisation, then runs the suite's own `setup` steps, for example creating templates.
2. **Cases.** It runs each case's steps in order. A case stops at its first failed step.
3. **Clean-up.** It deletes everything the suite created, newest first, which ends with the organisation. Clean-up also runs after failures and after a cancel. Whatever cannot be deleted is listed as *left over* on the run's page and in the history.

Suites with `fixture: none` create nothing; the health checks are an example.

**Several gateway instances.** A change made through the management API reaches the other instances within a second or two, and the next call may land on any of them. After every successful change the tester therefore waits, at most five seconds, until every running instance reports the new configuration version. A run against a single core does not wait.

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
| `gemini` | The Gemini interface: `generateContent` to Gemini, Anthropic and OpenAI-dialect models; a stream as server-sent events and as one JSON array; `countTokens` on Gemini and Anthropic (and refused on an OpenAI-dialect model); the SDK's Vertex AI paths with another project; Gemini's objects pointed to proxy mode. Mock only: embeddings in OpenAI's and Gemini's shapes, Vertex AI's express mode (a managed call, proxy mode's pinning, model routing refused), and xAI's reasoning billed outside the completion |
| `providers` | Gemini's own API through all three interfaces; a tool loop through the Anthropic interface whose second turn works only when the gateway carries the thought signature back (scripted against the mock, and live); the same loop with the OpenAI-dialect model, which for DeepSeek and Kimi needs the first turn's reasoning back and for Gemini's compatible endpoint its signature |

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
    needs: [openai]              # openai, anthropic, gemini, mock_control
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
| `model.openai`, `model.anthropic`, `model.gemini` | The model names, as calls use them |
| `model_id.openai`, `model_id.anthropic`, `model_id.gemini` | The ids in the model repository |
| `provider.openai`, `provider.anthropic`, `provider.gemini` | Provider names, as in `/proxy/{provider}/…` |
| `upstream.openai`, `upstream.anthropic`, `upstream.gemini` | The provider's own model ids |
| `model.embedding`, `model.xai`, `model.vertex` (and their `model_id`, `provider`, `upstream`) | Mock mode only: the mock's embedding model, xAI model and Vertex AI express-mode model |

Variables saved in `setup` are visible to every case. Variables a case saves stay inside that case.

### Checks

| Under `expect` | Checks |
|---|---|
| `status` | A status, or a list of accepted ones |
| `headers` | `{name: check}`. Header values are text, so `"1"` and `1` compare equal. |
| `json` | `{path: check}`. A path such as `choices.0.message.content`; `-1` is the last element; `*` maps over a list, as in `data.*.id`; `[key=value]` takes the first element whose member `key` is the text `value`, as in `content.[type=tool_use].id`. `save` takes the same paths. |
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

## 6. Sending and tracing a message

**Platform → Testing → Message** builds one request to the data plane, sends it as a workspace, and shows what happened to it, step by step.

**Composing.** Every part of the request can be set:
- **Send as:** the organisation and workspace, and the key (see below).
- **Interface:**
  - OpenAI chat completions, Anthropic messages, Gemini's `generateContent` (with the paths of the SDK's Gemini API mode or of its Vertex AI mode), native chat, or OpenAI's embeddings (each message one text);
  - proxy to a provider, given the provider and the path below it;
  - the model list;
  - or any method and path.
- **Conversation:** a system prompt and any number of user and assistant messages.
- **Parameters:** max tokens, temperature, top_p, stop sequences, streaming (with usage on the last chunk for OpenAI), and additional body fields as a JSON object (tools, response_format, reasoning, …).
- **Gateway options:** template, end user, session, trace id, agent, user, task, complexity, tags, timeout, attempts, unsupported parameters, prompt cache and audit. They are sent as `x-gw-*` headers, or, for the OpenAI, Gemini, native and embeddings interfaces, in the body's `gateway` block. A W3C `traceparent` can be generated.
- **More headers:** any others, which replace those the form sets. The key headers are left to the tester.

The panel on the right shows the exact request. **Edit the body by hand** takes the generated body as a starting point and sends whatever you write, even a body that is not valid JSON. Turning it off discards the edits.

**The key.** By default the tester creates a key in the workspace, `tester-trace-<message>`. It is used for this message only, deleted right after, and expires within ten minutes should the tester stop in between. To test what a particular key may do, choose **A key of my own** and paste it: it is used for this message and is never stored, logged or shown again. Calls with wrong keys count towards the data plane's block of addresses that send many bad keys (60 a minute); that block would also stop test runs for a while.

A message is a real call: it is admitted against the workspace's limits, counted and billed. For example, an end user's daily limit applies to it.

**The trace** shows five steps:
1. **What was sent:** the URL, headers (the key masked) and body.
2. **What the gateway planned:** the workspace's preview for the OpenAI, Anthropic and native interfaces (the Gemini interface and embeddings have none), covering route, provider, model requested and served, template, provider URL, dropped parameters, the body the provider would receive, and routing. The preview runs as the workspace without a key's restrictions. What actually happened is in step 4.
3. **What came back:** the status, the gateway's `x-gw-*` headers and the other headers, and when the response head and the first body byte arrived. For a stream, a strip shows when each event arrived, with the events in a table, followed by the body.
4. **The request log:** the call as the analytics service recorded it, with where the time went (client, gateway, connection, provider), routing, attempts and failovers, tokens and cost. It appears within a few seconds; the page waits up to 45 s.
5. **The audit record:** what the gateway changed in the body, and the four parts (client request, provider request, provider response, client response) with the resolution and the outcome. It needs **Ask for an audit record** (`x-gw-audit`) and a workspace whose audit mode is *on request* or *always*. Reading it is noted in the audit log.

**Edit and send again** opens the composer with the message's form, without the key. **Open in Insight** shows the call on the Requests page.

The last 100 messages are kept in the tester's volume (`/data/messages`), with the secrets masked.

## 7. Configuration

| Variable | Where | Meaning |
|---|---|---|
| `TESTER_URL` | console | The tester's address, for example `http://tester:8095`. Without it, the Testing page says testing is not set up. |
| `PGW_CORE_ADMIN_URL` | tester | The management listener, for example `http://core:8081` |
| `PGW_CORE_DATA_URL` | tester | The data plane, for example `http://core:8080`. With several gateway instances, the load balancer: `http://caddy:8088` in the scale layout. |
| `TESTER_MOCK_URL` | tester | The mock provider as the gateway reaches it. Empty: no mock mode. |
| `TESTER_MOCK_CONTROL_URL` | tester | The mock as the tester reaches it. Defaults to `TESTER_MOCK_URL`. |
| `TESTER_DATA_DIR` | tester | Runs and uploaded suites (default `/data`) |
| `TESTER_KEEP_RUNS` | tester | How many runs the history keeps (default 100) |
| `TESTER_LISTEN` | tester | Listen address (default `0.0.0.0:8095`) |
| `PGW_AUTH` | tester | `local` or `oidc`, as the core has it. With `oidc` a run works with a delegated grant. |
| `PGW_ID_INTERNAL_URL`, `TESTER_ID_CLIENT_SECRET` | tester | pacod-id's internal listener and the tester's client secret there, for the delegated grants (`oidc` only). |
| `TESTER_LOAD_AGENTS` | tester | The load agents, `name=URL` separated by commas, for example `local=http://load-agent:8096,eu-1=http://10.0.0.21:8096`. Empty: no load tests. |
| `TESTER_LOAD_TOKEN` or `TESTER_LOAD_TOKEN_FILE` | tester | The token the tester and the agents share, at least 32 characters. Required with `TESTER_LOAD_AGENTS`. |
| `TESTER_LOAD_MOCK_URL` | tester | The mock provider of load tests, as the gateway reaches it. Defaults to `TESTER_MOCK_URL`. Empty: no mock target. |
| `TESTER_LOAD_LIVE_MAX_CALLS` | tester | The most calls a load test against a live model may make (default 1000) |
| `TESTER_LOAD_MAX_DURATION` | tester | The longest a load test may last, in seconds (default 3600) |
| `TESTER_KEEP_LOAD_RUNS` | tester | How many load tests the history keeps (default 50) |

With Helm, the values under `tester:` cover the same settings. `tester.mockUrl` turns on mock mode, and `tester.persistence` keeps the history on a PVC. Load tests have values of their own (section 9.6).

## 8. Working on the tester

```sh
make tester-test      # ruff and pytest in tester/ (needs uv)
make run-tester       # the tester on the host, against scripts/dev-core.sh, on 127.0.0.1:28095
```

In the development stack (`make dev-up`), the tester is published on `127.0.0.1:18095` with mock mode on. The tests fake the core with `httpx.MockTransport` (`tester/tests/fakecore.py`), and the load agents the same way (`tester/tests/fakeagent.py`), so they need no running services.

The development stack also has a load agent (`local`, `127.0.0.1:18096`) and the load tests' mock (`127.0.0.1:19001`). `make load-agent-dev-up` starts a second agent that plays another machine (`remote`, `127.0.0.1:18196`). The agent's code is `crates/pacod-gw-load`; the JSON between tester and agent is tested on both sides against the fixtures in `crates/pacod-gw-load/tests/contract/`.

## 9. Load tests

A load test puts load on a gateway and measures what comes back: how many calls a second it
serves, how long they take, what fails, and what the gateway says it adds. Platform admins
configure it in the console under **Platform → Testing → Load**, choose the agents that make
the load, and follow it second by second.

### 9.1 How it fits together

```text
console ──► tester ──(management API: set-up, monitoring/live)──► core :8081
              │  plan, start, report every second, cancel  (Bearer TESTER_LOAD_TOKEN)
              ├──► load agent "local"   (in the stack)       ──► core :8080 or caddy :8088
              └──► load agent "eu-1"    (another machine)    ──► https://api.example.com
                         an external target instead:         ──► the URL of the test
```

- **The tester decides, the agents make the load.** The tester checks the request, sets up
  what the target needs, sends each agent its part, starts them together, and adds up what
  they report. An agent only makes calls and counts them.
- **The load agent is a container of its own** (`pacod-gw-load-agent`, `crates/pacod-gw-load`).
  It is written in Rust: an agent in Python would measure itself at thousands of calls a
  second. It serves the tester on port 8096.
- **Agents can run anywhere the gateway can be reached**: in the stack, or on other machines
  (section 9.6). Several agents can share one test; the load is split evenly.
- **An agent needs only the data plane.** The tester sets up the organisation, workspace and
  key, and hands the agents a key that works for this test only. Agents never get admin
  credentials.

### 9.2 Targets

| Target | Calls go to | What the tester sets up | Caps |
|---|---|---|---|
| Mock provider | This gateway, in front of a mock with the latency you set | A scratch organisation, workspace and key; the `tester-load-mock-*` providers and models | The agents' own |
| Live model | This gateway and a model that is set up | A scratch organisation, workspace, key and a call limit | At most `TESTER_LOAD_LIVE_MAX_CALLS` calls; the cost must be acknowledged |
| Own key | This gateway, with a key of an existing workspace | Nothing | A number of calls is required; the cost must be acknowledged |
| External gateway | Any URL: a minimal gateway (pacod-gw-lite), another installation | Nothing | A number of calls is required; the cost must be acknowledged |

**The mock.** The load mock (`load-mock`) is a mock provider that keeps no requests. The tester
writes the latency profile of each test into the URLs of its three providers,
`tester-load-mock-openai`, `-anthropic` and `-gemini`, as
`…/x-mock/delay=200,chunk_delay=20,chunks=20/v1`. Managed mode forwards no `x-mock` header, but
calls every path below a provider's base URL, and the mock reads options from there. These
providers and their models (`free: true`) are kept between tests, as the functional tests keep
theirs: the gateway deletes only models that were never active. A streamed call takes at
least `delay + (chunks − 1) × chunk delay` at the mock. The chunk delay also falls between a
stream's other events, so an Anthropic stream (message start, ping, block start and stop) takes
a few delays longer. The rest is the gateway and the network.

**Live models** cost money. Each call is made once (`x-gw-retries: 1`). The scratch workspace
gets a limit at the test's number of calls, as a backstop, and the key a concurrency cap.

**Own key and external.** The tester cannot know what is behind the key, so a number of calls
is required, but it has no ceiling beyond the agents' own. A minimal gateway in front of a mock
can take a hundred thousand calls. The key goes to every chosen agent and stays in its memory
until the test ends. The test keeps only a hint of it: a gateway key's id (`pgw_dev_…`), or the
start of its SHA-256. In proxy mode the test can send `x-mock` options, which proxy mode
forwards, for a mock behind the external gateway.

**Every call counts.** The number of calls includes one probe call per agent, and on this
gateway's scene the tester's own call before the load. The agents' shares add up to the rest.

### 9.3 The load

| Setting | Meaning |
|---|---|
| Calls at a time | A closed loop. Each client starts its next call when its last one has ended. It shows what a given number of clients gets through. A slow gateway slows its clients down with it. |
| Calls a second | An open loop. Calls start on time, whether or not earlier ones have ended, as independent clients would. A slow gateway shows as calls piling up. Above *calls in flight*, a call that is due is not started; it is counted as *not started*. |
| Stages | Each stage moves the target in a straight line from where the previous one ended (0 at the start). A stage of 0 seconds jumps. The form offers a steady load, a ramp up and down, and four steps. |
| Timeout | How long one call may take; longer counts as *timed out*. |
| Interface, mode | OpenAI chat completions, Anthropic messages, Gemini's own API, native chat, or embeddings; managed or proxy mode. Streamed answers by default. |
| HTTP | HTTP/1.1 by default: one call per connection, as most clients do. HTTP/2 carries many calls over a few connections. |

### 9.4 What a load test does

1. **Checks.** The tester checks the request. It asks every chosen agent whether it is there,
   speaks the same contract, is free, and allows its share of the load.
2. **Set-up** (mock and live). It writes the latency profile, creates the scratch
   organisation, workspace and key, and makes one call itself, to see that the target
   answers.
3. **Prepare.** It sends every agent its part, all at once. Each agent opens its connections
   and makes one probe call. If an agent's probe is refused (401 or 403), or does not succeed,
   the test ends before any load.
4. **Load.** It starts the agents together. Once a second it asks each agent for its last
   seconds, adds them up into one point, keeps it and streams it to the console. Every few
   seconds it reads the gateway's own figures (`/api/v1/monitoring/live`): calls in flight
   across all instances, and dropped usage events.
5. **End.** The test ends with its last stage, or when its calls are used up. Running calls
   get 30 seconds to finish. A cancel stops the agents at once.
6. **Clean-up.** The tester deletes what it set up, the key first. Whatever cannot be deleted
   is listed as left over.

**The lease.** Every report renews an agent's lease (30 seconds). An agent that is not asked
stops its part on its own. A tester that restarts in the middle of a test therefore leaves no
load running; the test is marked *interrupted*, with its scratch objects listed as left over.

### 9.5 What the page shows

- **Figures.** Calls, the share that succeeded, calls a second (on average and in the busiest
  second), time to the first byte and the whole call (p50 and p99), and the gateway's own
  delay.
- **Charts**, one point a second:
  - successful and failed calls, and calls not started;
  - time to the first byte and the whole call, p50, p95 and p99;
  - the gateway's own delay (`x-gw-delay-ms`): what the gateway adds to the provider's time,
    as it measures it. pacod-gw and pacod-gw-lite send it; other gateways do not;
  - calls in flight at the agents and, on this gateway, at the gateway;
  - the busiest agent's CPU, in percent of its CPUs, and the agents' scheduler lag.
- **The answers**: calls by status, and calls without a complete answer (could not connect,
  timed out, broke off, stopped with the test).
- **Warnings**, for figures to read with care:
  - an agent near its CPUs, or starting calls late, measures itself rather than the gateway;
  - calls not started mean the target did not keep up with the rate;
  - 503 is an overloaded gateway (`gateway_overloaded`) or an unavailable provider;
  - 429 is a rate limit or a concurrency cap;
  - 402 is a quota, such as a live test's call limit;
  - dropped usage events mean analytics did not keep up.

Latencies are of the successful calls only: a quick 429 says nothing about how fast the
gateway serves. They come from histograms with buckets about 1.6% wide, which add up across
seconds and agents; a percentile is within 0.8% of the exact one. The gateway's own calls in
flight are summed over instances that report every few seconds, so they trail the agents'
figure.

### 9.6 Agents

| Where | How | The tester reaches it at |
|---|---|---|
| Development stack | Always there (`make dev-up`), named `local`. `make load-agent-dev-up` adds `remote`, which reaches the stack only through its published ports, as another machine would. | `http://load-agent:8096`, `http://host.docker.internal:18196` |
| Production, in the stack | `docker-compose.load.yml` ("make up LOAD_TEST=1"), with `TESTER_LOAD_TOKEN` in deploy/.env. Off by default. | `http://load-agent:8096` |
| Another machine | `deploy/load-agent/`: copy `.env.example` to `.env`, set the token and the gateway's URL as the machine reaches it, `make load-agent-up`. List it in `TESTER_LOAD_AGENTS` on the central host. | its private address, port 8096 |
| Kubernetes | `loadAgent.enabled` and `loadMock.enabled`, with `secrets.loadToken`; agents elsewhere in `tester.loadAgents`. | the Service `<release>-load-agent` |

**An agent in the stack shares the gateway's machine.** Its CPU is taken from the gateway, and
Docker's network adds time. Its figures show behaviour, not capacity. For capacity, use agents
on other machines, aimed at the public API host, and watch their CPU warnings. An agent allows
up to 10 000 calls at a time by default (`LOAD_AGENT_MAX_CONCURRENCY`); how many it can drive
depends on its CPUs, and it needs a `nofile` limit of one socket per call in flight (the compose
files set 65 536).

In the scale layout, the agent in the stack loads the balanced data plane
(`LOAD_AGENT_TARGET_URL=http://caddy:8088`); the development scale layout sets this.

Each agent has caps of its own, whatever a test asks for: `LOAD_AGENT_MAX_CONCURRENCY`
(10 000), `LOAD_AGENT_MAX_RATE` (5 000 a second) and `LOAD_AGENT_MAX_DURATION` (3 600 seconds).
`crates/pacod-gw-load/README.md` lists the agent's variables and its API.

### 9.7 Security

- **The token.** Every request to an agent needs the token it shares with the tester, compared
  in constant time. It is at least 32 characters, and never a command-line argument.
- **Agents are off by default in production.** An agent makes load on request.
- **External URLs are free.** A platform admin can aim the agents at any URL they can reach.
  The run record keeps who did it and where. The agent in the stack is kept off the network of
  the databases. It reaches only the core, Caddy's internal data plane and the outside
  (`docs/threat-model.md`).
- **Keys.** A scratch key works for the test's mode and model only, expires 15 minutes after
  the test should have ended, and is deleted first. A pasted key stays in the agents' memory
  for the test; the tester stores a hint of it only.
- **Plain HTTP.** The tester talks to agents in plain HTTP, and the token and the test's key
  cross that link. Agents on other machines need a private network or a VPN, as gateways on
  other machines do.

### 9.8 From the command line

`make load PROFILE=managed` (`gw-load`) runs a fixed series against the development stack:
calls straight to the mock, then the same calls through the gateway, and prints what the
gateway added. It uses the agents' engine, so both measure the same way.
