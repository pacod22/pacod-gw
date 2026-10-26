# The minimal gateway (pacod-gw-lite)

`pacod-gw-lite` is the gateway's data plane on its own: one process in one container, without
PostgreSQL, ClickHouse, console, management API or metrics. Its configuration is one `.env`
file. Every call is one line in its log. It is a separate option beside the full installation;
the two share nothing but the name `pacod-gw` on the shared network, so one host runs one of
them (section 9).

**State of this document.** Written with the first version, 2026-10-14, on Docker Desktop for
macOS. What ran:

- The production compose file with a scratch `deploy/lite/.env` (fake keys, providers pointed
  at the mock): `keygen` and `make lite-check` through Compose, `make lite-up`, calls through
  both client APIs and a pool, the JSON log, `make lite-down`. Not yet on a server.
- The development stack (`make lite-dev-up`, `make lite-smoke`): every check passed and every
  call was found in the log, at the same time as the full gateway's development stack and its
  own smoke test.
- The binary on the host against OpenAI and Anthropic (`make lite-live`): both client APIs,
  translation in both directions, a stream, a pool, proxy mode, each call logged with its cost
  and no key in the log.

## 1. What it is, and what it is not

Not to be confused with the full gateway's *minimal setup* (the core with PostgreSQL but
without the analytics service, runbook and concept section 17): that is still the full gateway,
with its console and database.

The request path is the full gateway's own code (`pacod-gw-core`): the same client APIs, the
same translation between them, the same retries, failover, circuit breaker and accounting.
What the minimal gateway leaves out is everything that needs a database or serves people.

| | Full gateway (`deploy/docker-compose.yml`) | Minimal gateway (`deploy/lite/`) |
|---|---|---|
| Containers | core, analytics, console, PostgreSQL, ClickHouse, Caddy, pacod-id, Prometheus, Grafana, … | one |
| Configuration | console and management API, stored in PostgreSQL, changed at run time | `.env`, read at start |
| Client APIs | OpenAI, Anthropic, Gemini (and Vertex AI), the gateway's own, proxy mode | the same |
| Translation, streaming, retries, failover, circuit breaker | yes | yes |
| Embeddings, speech, token counting, model lists | yes | yes |
| Anthropic's and OpenAI's files and batches (`/v1/files`, `/v1/messages/batches`, `/v1/batches`) | through the gateway, with a record of who owns what, in PostgreSQL | through the gateway, with that record in a directory of its own (`PGW_OBJECTS_DIR`, section 14); 501 without it |
| Gateway keys | per workspace, created in the console | listed in `.env` |
| Organisations, workspaces, members, roles | yes | one implicit organisation |
| Pools of models (templates) and routing schemes | yes, with rules | pools with an algorithm, no rules |
| Limits | every kind and window, shared by all instances, kept across restarts | per key: requests per minute, tokens per minute, calls at once; in memory, per instance |
| Costs | prices tracked automatically, reports | prices from `.env`, cost in each log line |
| Request log, reports, audit mode | ClickHouse, console | the container's log; bodies on request |
| Metrics, traces, alerts | Prometheus, OpenTelemetry, Alertmanager | none |
| Plugins | the intent plugin per organisation and workspace, set in the console; routing and limits by intent | the intent plugin for every client, from a catalogue file; block and flag (section 12) |
| Response cache | on by default, per platform, organisation and workspace; answers shared by all instances in Valkey | on by default, for every client; answers in this instance's memory (section 13) |
| TLS | Caddy | none: put a TLS proxy in front where calls cross a network |

## 2. Quick start

```sh
cp deploy/lite/.env.example deploy/lite/.env
chmod 600 deploy/lite/.env

# A key for each application that calls the gateway; paste the line into deploy/lite/.env.
docker run --rm pacod-gw-lite:0.1.0 keygen app
#   PGW_CLIENT_APP_KEY=pgw_app_…

# Fill in the provider keys and the models, then let the gateway read the file.
make lite-check        # what it would serve, without secrets; fails if it would not start
make lite-up           # docker compose -f deploy/lite/docker-compose.yml up -d --wait (after make network)

curl http://127.0.0.1:14080/v1/chat/completions \
  -H "authorization: Bearer pgw_app_…" -H "content-type: application/json" \
  -d '{"model": "anthropic/claude-sonnet-5-5", "messages": [{"role": "user", "content": "Hello"}]}'

docker compose -f deploy/lite/docker-compose.yml logs -f pacod-gw
```

The image comes from `make images` (all images) or
`docker build -f deploy/docker/rust.Dockerfile --target lite -t pacod-gw-lite:0.1.0 .`.

The OpenAI and Anthropic SDKs need only their base URL and key changed:
`base_url="http://127.0.0.1:14080/v1"` (OpenAI) or `"http://127.0.0.1:14080"` (Anthropic), and
the client key as the API key. An application that called LiteLLM with its master key and
model names like `anthropic/claude-sonnet-5-5` keeps both: give it a client key and name the
same models.

Without Docker, the binary reads the same file: `make run-lite` runs
`cargo run -p pacod-gw-lite -- --env-file deploy/lite/.env`.

## 3. Configuration

Everything is an environment variable. In a container they come from `deploy/lite/.env`, which
Compose passes in whole; on a host, `--env-file` names the file, and variables set in the
process win over it.

Rules:

- An object is a group of variables `PGW_<KIND>_<LABEL>[_<ATTRIBUTE>]`. The kinds are
  `PROVIDER`, `MODEL`, `POOL` and `CLIENT`. The label is capital letters and digits only, no
  underscore: whatever follows the first underscore after it is the attribute, so a misspelt
  attribute stops the start instead of creating a new object.
- An empty value counts as unset.
- Values are taken literally. `$$` stands for `$` (outside single quotes), because Compose
  reads a single `$` as the start of a variable. Unquoted values end at ` #`.
- The gateway reads everything once at start. Every problem is listed together, and the
  gateway does not start until there is none. `pacod-gw-lite check` (`make lite-check`) shows
  what it read.
- `PGW_LITE_BIND`, `PGW_LITE_PORT`, `PGW_LITE_ENV_FILE`, `PGW_REGISTRY` and `PGW_VERSION` are
  for Compose; the gateway ignores them. Any other `PGW_` variable it does not know is named in
  a warning at start (a full gateway's `.env` holds many).
- The intent plugin's variables, `PGW_INTENT_*`, and the response cache's, `PGW_CACHE_*`, are
  single settings, not objects (sections 12 and 13).

### 3.1 Providers

The label in lower case is the provider's name, as in model names (`anthropic/…`) and proxy
paths (`/proxy/anthropic/…`). A name that is one of the presets needs only its key: `openai`,
`anthropic`, `gemini`, `gemini-openai`, `vertex`, `vertex-express`, `glm`, `glm-cn`,
`deepseek`, `xai`, `kimi`, `kimi-cn`, `groq`, `jina`, `voyage`, `cohere`, `ollama`,
`ollama-cloud`. The runbook's section on
providers says what each preset does.

| Attribute | Meaning |
|---|---|
| `_KEY` | The provider's API key. Required, except for `ollama`. |
| `_KEY_FILE` | Or the path of a file holding it, for a mounted Docker secret. Vertex AI: the service account's JSON key. |
| `_PRESET` | The preset, when the name is not one. |
| `_NAME` | The name, when it needs a `-` (the label cannot hold one). |
| `_BASE_URL` | Another endpoint than the preset's: GLM's coding plan, a Vertex AI project, a local server. |
| `_TIMEOUT_SECONDS` | How long to wait for the provider, 1 to 600 (default `PGW_TIMEOUT_SECONDS`). |

```sh
PGW_PROVIDER_OPENAI_KEY=sk-…
PGW_PROVIDER_GOAI_NAME=gemini-openai          # Gemini's OpenAI-compatible endpoint
PGW_PROVIDER_GOAI_KEY=…
PGW_PROVIDER_LOCAL_PRESET=ollama              # a model server on this host
PGW_PROVIDER_LOCAL_BASE_URL=http://host.docker.internal:11434/v1
PGW_EGRESS_ALLOW_PRIVATE=host.docker.internal # private addresses are refused otherwise
```

### 3.2 Models

`PGW_MODEL_<LABEL>` holds the name clients send as `model`. A name
`<provider>/<model id at the provider>` needs nothing else. Its kind and capability preset are
guessed from the model id as the full gateway's discovery does, and for Claude models by their
generation (a thinking budget up to Haiku 4.5 and Sonnet 4.x, Sonnet 5.5's own preset, always
thinking for Opus 5.5 and Fable 5.1). `check` shows the guess; set `_CAPABILITIES` where it is
wrong, for a fine-tuned or a new model for example.

| Attribute | Meaning |
|---|---|
| `_PROVIDER` | The provider, when the name does not start with it (`gemini-3.8-flash`). |
| `_UPSTREAM` | The model's id at the provider, when it is not the rest of the name. |
| `_KIND` | `chat`, `embedding`, `speech`, `image`, `transcription`, `rerank`, `realtime` or `decision` (Jev; section 12). Guessed from the model id when left out. |
| `_CAPABILITIES` | The capability preset: what the model accepts (reasoning style, sampling parameters, tools, images). See the runbook's model section; `openai-chat`, `openai-reasoning`, `anthropic-adaptive`, `gemini-3-flash`, `deepseek`, `glm-5`, `embedding`, `speech` and others. |
| `_CONTEXT`, `_MAX_OUTPUT` | Context window and most output tokens. |
| `_PRICE` | Rates for the cost in the log, per million tokens: `input`, `output`, `cache_read`, `cache_write_5m`, `cache_write_1h`, `reasoning`, and `input_audio`, `output_audio`, `input_image`, `output_image` where audio or image tokens are priced apart. Per unit: `per_character` (speech), `per_image`, `per_audio_second` (transcription), `per_search_unit` (rerank), `per_request`, `per_server_tool_call`. Or `free`. |
| `_CURRENCY` | The price's currency (default `PGW_CURRENCY`, USD). |

```sh
PGW_MODEL_SONNET=anthropic/claude-sonnet-5-5
PGW_MODEL_SONNET_PRICE=input=3,output=15,cache_read=0.3,cache_write_5m=3.75
PGW_MODEL_FLASH=gemini-3.8-flash
PGW_MODEL_FLASH_PROVIDER=gemini
```

Only configured models can be called, in managed mode (`/v1/chat/completions`,
`/v1/messages`, …) and in proxy mode alike (section 6).

### 3.3 Pools

`PGW_POOL_<LABEL>` lists models, and clients call the pool as the model `@<label in lower
case>`. Section 5 describes how a pool chooses.

| Attribute | Meaning |
|---|---|
| `_NAME` | The name, when it needs a `-`. |
| `_ALGORITHM` | `priority` (default), `weighted`, `round_robin`, `weighted_round_robin`, `least_in_flight`, `lowest_latency`, `cheapest`. |
| `_WEIGHTS` | One whole number per model, for the weighted algorithms; 0 keeps a model as backup only. |
| `_TIMEOUT_SECONDS` | How long each model gets, up to 600. |
| `_RETRIES` | Attempts per model, 1 to 5 (default 1: the next model is the better retry). |

### 3.4 Clients

Each application that calls the gateway is a client with its own key. The label in lower case
is the client's name, which every log line shows.

| Attribute | Meaning |
|---|---|
| `_KEY` | The key, `pgw_<name>_<secret>` (section 4). |
| `_KEY_FILE` | Or a file holding it. |
| `_RPM`, `_TPM` | Requests and tokens per minute. |
| `_CONCURRENCY` | Calls at the same time. |
| `_MODELS` | What the client may call, comma separated: models and `@pools`. Listing a pool allows its models. Unset: everything. |

### 3.5 Other settings

| Variable | Default | Meaning |
|---|---|---|
| `PGW_LISTEN` | `0.0.0.0:8080` | Where the gateway listens. In Compose the container's address; `PGW_LITE_BIND` and `PGW_LITE_PORT` (127.0.0.1:14080) publish it. |
| `PGW_LOG_FORMAT` | `json` | `json` or `text`, any case; anything else is `json`, with a warning at the start (section 8). `deploy/lite/.env.example` sets `text`. |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`; `warning` is `warn`) or a filter (`info,pacod_gw_core=debug`). Anything else is `info`, with a warning. |
| `PGW_LOG_BODIES` | `off` | `off`, `on` or `on_request` (section 8). |
| `PGW_LOG_BODY_MAX_BYTES` | 16384 | Bodies are cut after this many bytes each. |
| `PGW_UNSUPPORTED_PARAMS` | `drop` | A parameter the model does not take: `drop` it (the response says so in `x-gw-dropped-params`) or `reject` the call. |
| `PGW_CURRENCY` | `USD` | Currency of prices that name none. |
| `PGW_TIMEOUT_SECONDS` | 180 | How long to wait for a provider; for a stream, until its first byte. |
| `PGW_STREAM_IDLE_SECONDS` | 120 | Seconds without a byte from the provider after which a stream ends. |
| `PGW_MAX_BODY_BYTES` | 33554432 | Largest request body. |
| `PGW_MAX_IN_FLIGHT` | 10000 | Calls in flight before `503 gateway_overloaded`. |
| `PGW_EGRESS_ALLOW_PRIVATE` | – | Hosts or CIDR ranges on private networks where providers may live. |
| `PGW_TRUSTED_PROXIES` | – | Proxies whose `X-Forwarded-For` is believed (addresses or CIDR). |
| `PGW_DRAIN_SECONDS`, `PGW_DRAIN_DELAY_SECONDS` | 30, 0 | On stop: how long running calls may finish, and how long to report "not ready" first. |

## 4. Client keys

A key looks like `pgw_<name>_<secret>`: `pacod-gw-lite keygen <name>` makes one, or
`printf 'pgw_<name>_%s\n' "$(openssl rand -hex 24)"`. Applications send it as
`Authorization: Bearer …`, `x-api-key` or `x-goog-api-key`, whichever their SDK uses.

The gateway keeps only a hash, made with a secret that is new at every start. The log names a
client by its name; a refused key is logged with the name part only (`key=app`), never the
secret.

To replace a key, change the variable and restart. To have two valid keys during a change,
give the application a second client for the time being.

A source address that sends 60 wrong keys in a minute is blocked for the rest of that minute.
When clients reach the container through a port Docker publishes on the same host, the gateway
may see all of them as the Docker network's gateway address (`client=172.18.0.1` in the log):
then one misconfigured application can lock out the others for a minute. Set
`PGW_TRUSTED_PROXIES` where a proxy in front passes on the real address.

## 5. Pools

A call to `@fast` goes through the pool's models. With `priority`, the first model serves and
the next one takes over on a 429, a 5xx or a lost connection, up to every model of the pool
(at most 8). The other algorithms pick the first model differently and fail over the same
way. A timeout does not move a call on: the provider may be generating, and billing, the
answer already. Failover happens only before the first byte of the answer has reached the
client.

A model that keeps failing is skipped for a while (the circuit breaker), and the log line says
which model served (`served=`), whether the call moved on (`failovers=`), and the route
(`route=pool-fast/default`).

## 6. Proxy mode

`/proxy/<provider>/<path>` relays a request to the provider as it is, with the provider's key
instead of the client's: `/proxy/openai/v1/chat/completions`, `/proxy/anthropic/v1/messages`,
`/proxy/anthropic/v1/files`. It reaches any path the provider's API offers. A request that
names a model must name a configured one (by the provider's model id, `gpt-4.1-nano`, or the
configured name) that the client may use; paths that name no model, such as model lists,
files and batches, are relayed as they are. Every call is still authenticated, limited,
logged and, where the response reports usage, priced.

The provider keys are treated as the operator's own: proxy mode also relays the provider's
files and batches endpoints, and a message may name a file the provider holds. **Clients that
share one minimal gateway can therefore see each other's files at the provider**, through
proxy mode, and through the managed endpoints too, since all clients belong to one workspace
(section 14). Give applications that must not, gateways (or provider keys) of their own.

## 7. Limits

`_RPM`, `_TPM` and `_CONCURRENCY` refuse a call with 429 and a `retry-after` header when the
client is over its limit. The windows slide over the last minute. Tokens are counted when a
call ends, so the call that crosses a token limit is served and the next one is refused.
Where a response reports no usage, the gateway estimates it from the bytes.

The counts live in memory: a restart starts them from zero, and two minimal gateways behind one
balancer each count on their own. `GET /gw/v1/quota` shows a client its limits and what is used.

## 8. Logging

The log follows the convention every pacod product shares (`pacod-demo/docs/LOGGING.md`): one
event per line on standard output, in JSON (`PGW_LOG_FORMAT=json`, the default) or text. A JSON
line starts with five keys, always in this order:

| Key | Content |
|---|---|
| `timestamp` | UTC, RFC 3339 with milliseconds. |
| `level` | `debug`, `info`, `warn` or `error`. |
| `service` | `SERVICE_NAME`, by default `pacod-gw`, the compose service. |
| `logger` | What the line is: `pacod_gw_lite::call`, `pacod_gw_lite::body`, `pacod_gw_lite::request`, or the module of any other message. |
| `message` | `call`, `body`, `rejected`, or the text of any other message. |

The line's fields follow at the top level, numbers as numbers. Every call that the gateway
accounts for is one line, logger `pacod_gw_lite::call`: served, failed, refused by a limit or
cancelled.

```json
{"timestamp":"2026-10-14T09:12:03.418Z","level":"info","service":"pacod-gw","logger":"pacod_gw_lite::call","message":"call","request_id":"gw_01a1…","key":"app","mode":"managed","interface":"openai","path":"/v1/chat/completions","model":"@fast","served":"anthropic/claude-sonnet-5-5","provider":"anthropic","pool":"fast","route":"pool-fast/default","status":"ok","http":200,"attempts":2,"failovers":1,"stream":false,"finish_reason":"end_turn","input_tokens":812,"output_tokens":96,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0,"usage":"provider","cost":0.003876,"currency":"USD","price_missing":false,"latency_ms":1840.2,"ttfb_ms":1702.5,"gateway_ms":0.6}
```

The text format holds the same in one line; a value with spaces, quotes or line breaks is quoted
and escaped as in JSON:

```text
2026-10-14T09:12:03.418Z INFO  pacod-gw pacod_gw_lite::call: call request_id=gw_01a1… key=app mode=managed interface=openai path=/v1/chat/completions model=@fast served=anthropic/claude-sonnet-5-5 provider=anthropic pool=fast route=pool-fast/default status=ok http=200 attempts=2 failovers=1 stream=false finish_reason=end_turn input_tokens=812 output_tokens=96 cache_read_tokens=0 cache_write_tokens=0 reasoning_tokens=0 usage=provider cost=0.003876 currency=USD price_missing=false latency_ms=1840.2 ttfb_ms=1702.5 gateway_ms=0.6
```

The fields of a call line:

| Field | Meaning |
|---|---|
| `request_id` | The call's id, also in the response header `x-gw-request-id`. |
| `key` | The client's name. |
| `mode`, `interface`, `path` | `managed` or `proxy`; the client API (`openai`, `anthropic`, `gemini`, `native`); the path. |
| `model`, `served`, `provider` | The model asked for, the one that served, its provider. |
| `pool`, `route`, `failovers`, `models_tried` | For a pool: its name, the scheme and rule, how often the call moved on, the models tried. |
| `status`, `http` | `ok`, `error`, `timeout`, `rejected` (refused by the gateway, a limit for example) or `cancelled` (the client left); the HTTP status. |
| `attempts`, `stream`, `finish_reason` | Provider requests made; whether the answer was streamed; why it ended. |
| `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `reasoning_tokens`, `usage` | Token counts, and where they come from: `provider`, `estimated`, `missing`, or `cache` for an answer from the response cache. |
| `cost`, `currency`, `price_missing` | The call's cost from the configured price; `price_missing=true` when the model has none. |
| `latency_ms`, `ttfb_ms`, `gateway_ms` | Total time, the provider's time to the first byte, the time the gateway itself added. |
| `error_type`, `error`, `upstream_request_id`, `end_user`, `session_id`, `dropped_params` | When there is one. |
| `intent`, `intent_confidence`, `intent_engine`, `verdict`, `plugin_error`, `plugin_ms` | When the intent plugin looked at the call (section 12): the intent and its confidence; the engine that decided (`llm`, `jev`, `cascade:jev`, `cascade:llm`); what the rules did (`allow`, `flag`, `block`, or in shadow mode `would_flag`, `would_block`); why no verdict came; how long the call waited for it (enforce mode only). |
| `cache`, `saved_cost` | What the response cache did (section 13): `hit`, `miss`, `would_hit`, `refresh` or `bypass`; on a hit, what the kept answer would cost now. A hit's `usage` is `cache`. |
| `task`, `parent_id` | On a classification's own line: `plugin.intent`, and the `request_id` of the call it classified. Its `key` is `plugin:intent`. |

Errors and timeouts are logged at `warn`, everything else at `info`.

Requests refused before a call is accounted for, a wrong key above all, are logged as
`rejected` lines with logger `pacod_gw_lite::request`: `method`, `path`, `client` (the address),
`http` (the status), `error_type` and `key` (the name part of the key). Model lists, quota
queries and health checks are logged at `debug` only. These lines keep the field names of the
call line; they are not the request lines of the convention (`<METHOD> <path> <status>` with
`status` and `client_ip`).

**Bodies.** `PGW_LOG_BODIES=on` adds a second line per call, logger `pacod_gw_lite::body`,
with the request the client sent and the response it received (`request`, `response`, their
sizes, `truncated`), each cut at `PGW_LOG_BODY_MAX_BYTES`. `on_request` does so only for calls
that send the header `x-gw-audit: true`. Credentials in headers are never logged; images and
documents are logged as size and hash. Bodies hold what users wrote: switch this on only where
that is allowed, and mind the size of the log. The compose file rotates the container's log at
5 × 10 MB, as every pacod service does: where calls must be kept longer, ship the log to a log
collector. A call that an intent rule flags has its bodies logged as well, unless
`PGW_LOG_BODIES` is `off` (section 12).

The gateway never logs provider keys or client keys.

## 9. Deployment

| | Production (`make lite-up`) | Development (`make lite-dev-up`) |
|---|---|---|
| Compose project | `pacod-gw-lite` | `pacod-gw-lite-dev` |
| Files | `deploy/lite/docker-compose.yml`, `deploy/lite/.env` | the same plus `docker-compose.dev.yml`; no env file |
| Gateway | `PGW_LITE_BIND:PGW_LITE_PORT`, default 127.0.0.1:14080 | 127.0.0.1:18280 |
| Mock provider | – | 127.0.0.1:19280; the intent plugin's classifier on `gw-mock-intent`, not published |
| Image | `pacod-gw-lite:$PGW_VERSION` | `pacod-gw-lite:dev`, built from the working tree |

The project name and ports are the minimal gateway's own. Its network is the shared `pacod-net`
of all pacod stacks (`pacod-demo/docs/NETWORK.md`), where it is `pacod-gw`, the name other pacod
products call the gateway by, and its development mock is `gw-mock`. So it does not run on the
same host as the full installation (project `pacod-gw`) or its development stack, which have a
`pacod-gw` and a `gw-mock` of their own; `make network` creates the network.

The image is 16 MB unpacked (the core's: 29 MB). The container runs as an unprivileged user
on a read-only file system with no capabilities; it writes nothing. The intent plugin's catalogue
is mounted read-only (section 12). Its health check calls `/readyz`. On `docker compose stop` it finishes the
calls it is serving (up to `PGW_DRAIN_SECONDS`) before it exits.

Without Compose:

```sh
docker run -d --name pacod-gw-lite --read-only --cap-drop ALL --env-file deploy/lite/.env \
  -p 127.0.0.1:14080:8080 pacod-gw-lite:0.1.0
```

`docker run --env-file` does not read quotes or `$$` as Compose does: keep values unquoted and
free of `$` there.

The gateway speaks plain HTTP. Where calls cross a network, put a TLS proxy in front and set
`PGW_TRUSTED_PROXIES` to its address. Values from `env_file` are visible in
`docker inspect`; the `_KEY_FILE` variables with Docker secrets avoid that. Never print
`docker compose … config` with a real `.env`: it shows the keys.

Several minimal gateways behind a balancer work, each with the same `.env`; their limits are
counted per gateway (section 7).

## 10. Endpoints

| Path | What |
|---|---|
| `POST /v1/chat/completions`, `POST /v1/messages`, `POST /v1/messages/count_tokens` | Chat in OpenAI's and Anthropic's API, any configured model or pool. |
| `POST /v1/embeddings`, `POST /v1/audio/speech` | Embeddings and speech models. |
| `POST /v1/images/generations`, `POST /v1/images/edits` | Image models: OpenAI-shaped providers, Imagen and Gemini's image models. |
| `POST /v1/audio/transcriptions`, `POST /v1/audio/translations` | Speech to text: OpenAI-shaped providers (OpenAI, Groq), and Gemini. |
| `POST /v1/rerank`, `POST /v2/rerank` | Rerank in Cohere's shape: Cohere, Jina, Voyage and servers that speak Jina's API (vLLM). |
| `POST /v1/systemone` | TypeSafe's System One API, for decision models (section 12). |
| `POST /v1/responses` | OpenAI's Responses API, stateless: relayed to OpenAI and xAI at their own addresses, translated for the other models (and for a preset pointed at another base URL). `store`, `previous_response_id` and `conversation` are refused. |
| `GET /v1/models`, `GET /v1/models/{id}` | The models and pools the client may use. |
| `/v1beta/models/…`, `/v1/projects/…`, `/v1/publishers/…` | Gemini's API as its SDKs call it, on AI Studio or Vertex AI. |
| `POST /gw/v1/chat`, `GET /gw/v1/models`, `GET /gw/v1/quota` | The gateway's own API. |
| `/proxy/{provider}/…` | Proxy mode (section 6). |
| `/v1/files…`, `/v1/messages/batches…`, `/v1/batches…` | Anthropic's and OpenAI's files and batches (section 14); 501 `not_available` without `PGW_OBJECTS_DIR`. |
| `GET /healthz`, `GET /readyz` | Liveness and readiness. |

Request headers the full gateway reads work here too: `x-gw-end-user`, `x-gw-session`,
`x-gw-unsupported-params`, `x-gw-audit` and the routing hints of its documentation.

## 11. Testing

| Command | What it checks |
|---|---|
| `cargo test -p pacod-gw-lite` | The configuration rules, the generated configuration, and the gateway end to end against the mock providers in process, without a database: both client APIs and translation, streams, proxy mode, pool failover, the limits, 401 and 501, the body log and the JSON log without secrets, the intent plugin with both engines (block, flag, shadow, the classification's own line), and the response cache (a repeated call at temperature 0 answered from memory, every client one workspace, off). |
| `make lite-dev-up` then `make lite-smoke` | The container against the mock (`gw-smoke lite`); then every call's line in the container's log, the chat call's intent and its classification's line, and no key in it. The development stack runs the intent plugin in shadow mode with the example catalogue; its classifier has a mock of its own (`gw-mock-intent`), so that failures scripted on `gw-mock` reach only the calls they are meant for. |
| `make lite-live` | The binary on the host against OpenAI and Anthropic with the keys in the root `.env`; each call's line with a cost, no key in the log. |
| `make deploy-check` | The compose files, among everything else in `deploy/`. |

**Load tests.** The minimal gateway has no console of its own, but a full installation's console
can put load on it: under **Platform → Testing → Load**, choose the target *External gateway*,
give the gateway's URL as the load agents reach it, a client key and a model
([testing.md](testing.md), section 9). The tester sets nothing up there; the agents only make
calls. Against the development stacks: `make dev-up` and `make lite-dev-up`, then the URL
`http://host.docker.internal:18280` and the key `pgw_dev_DevelopmentOnlyKey0123456789`.
Load tests send `x-gw-cache: off`, so that repeating the same request measures the provider,
not the response cache; a test can name the header itself to measure the cache.

To give a mock provider behind the minimal gateway a latency profile:
- **Managed mode:** put the options into the provider's base URL; the mock reads them from
  the path:
  `PGW_PROVIDER_OPENAI_BASE_URL=http://gw-mock:9000/x-mock/delay=200,chunk_delay=20/v1`.
- **Proxy mode:** the load test sends them as the `x-mock` header, which proxy mode forwards.

The call log writes one line per call. At thousands of calls a second that costs the gateway
CPU, and its figures include it. The development stack also classifies every call with the
intent plugin in shadow mode (section 12): each call makes a second call to `gw-mock-intent`
and writes a second log line, and its own line waits for the verdict. Set `PGW_INTENT_MODE: off`
in `docker-compose.dev.yml` to measure the gateway without it. The gateway's own delay (`x-gw-delay-ms`) is charted as for the
full gateway. Calls in flight at the gateway are not, because the minimal gateway reports no
figures. A client's limits (`_RPM`, `_CONCURRENCY`) apply to the test's key and show as 429.

## 12. Intent plugin

The full gateway's intent plugin ([plugin-concept.md](plugin-concept.md)) runs in the minimal
gateway too. It classifies what each call asks for against a catalogue of intents, logs the
intent with the call, and blocks or flags calls by the catalogue's rules. It is off unless
`PGW_INTENT_MODE` is set. One catalogue and one setting apply to every client.

| Variable | Default | Meaning |
|---|---|---|
| `PGW_INTENT_MODE` | `off` | `shadow`: classify alongside the provider call. Nothing is blocked; the log says what the rules would have done. `enforce`: classify first; the rules block and flag. |
| `PGW_INTENT_FILE` | – | The catalogue: YAML, or JSON when the name ends in `.json`. Required when the mode is set. |
| `PGW_INTENT_ENGINE` | `llm` | `llm`: a chat model classifies. `jev`: TypeSafe's Jev. `cascade`: Jev first; the chat model decides when Jev's confidence is below 0.6 or it answers `unknown`. |
| `PGW_INTENT_MODEL` | – | `llm` and `cascade`: the chat model or `@pool` that classifies. Its capability preset must offer JSON-schema output. |
| `PGW_INTENT_JEV_MODEL` | the only decision model | `jev` and `cascade`: the decision model. |
| `PGW_INTENT_ON_ERROR` | `closed` | Enforce mode, when no verdict comes in time: refuse the call (`503 plugin_unavailable`), or let it through (`open`). |
| `PGW_INTENT_TIMEOUT_MS` | 3000 (`llm`), 1000 (`jev`), 4000 (`cascade`) | How long a call waits for the verdict, 50 to 60000. |
| `PGW_INTENT_EXPOSE` | `false` | `true` tells clients the intent in the response header `x-gw-intent`. |

**The catalogue.** `deploy/lite/intents.example.yaml` is a start. The format is the console's:
a `name`; the `intents`, each with a `description`, up to ten `examples` and what it is `not_for`;
further `questions` (flags, scores, choices); and `rules`, each `when` (intents, confidence,
flags, scores, choices) then `block`, `flag` or `allow`. `unknown` is added for calls that fit no
intent. `check` reads the file and reports what is wrong with it, as the console would. Mount the
file read-only into the container and name its path there:

```yaml
# deploy/lite/docker-compose.yml, under the pacod-gw service
    volumes:
      - ./intents.yaml:/etc/pacod-gw/intents.yaml:ro
```

```sh
PGW_INTENT_MODE=shadow
PGW_INTENT_FILE=/etc/pacod-gw/intents.yaml
PGW_INTENT_MODEL=openai/gpt-5.6-luna
```

**Jev.** TypeSafe is a provider, and Jev a model of kind `decision`. Clients ask it their own
questions at `POST /v1/systemone`, or through proxy mode at `/proxy/typesafe/v1/systemone`; it
does not chat, and model lists leave it out. A client without `_MODELS` may call it, as it may
every model: list the models of a client that must not send text to TypeSafe. TypeSafe charges input tokens only but reports output tokens, so the
output price is 0, not missing:

```sh
PGW_PROVIDER_TYPESAFE_KEY=…
PGW_MODEL_JEV=typesafe/jev-1.13.0
PGW_MODEL_JEV_PRICE=input=0.042,output=0
PGW_INTENT_ENGINE=jev
```

Jev sends the classified text to TypeSafe, a third party in the United States. Setting the
engine to `jev` or `cascade` is the approval that the full gateway asks of its platform
administrators. The `llm` engine sends it to the provider of the classifying model.

What happens to a call:

| | Shadow | Enforce |
|---|---|---|
| Classified | alongside the provider call; the log line waits for the verdict | before the call is planned; `plugin_ms` is the wait |
| A `block` rule matches | `verdict=would_block`; the call goes on | `403` in the client's dialect with the rule's message, `x-gw-error-type: intent_blocked`; the log line says `status=rejected verdict=block` |
| A `flag` rule matches | `verdict=would_flag` | `verdict=flag`; the call goes on, and its bodies are logged unless `PGW_LOG_BODIES=off` |
| No verdict in time | `plugin_error` set; the call goes on | `PGW_INTENT_ON_ERROR` decides |

- Each classification is a call of the gateway's own: a log line with `task=plugin.intent` and
  the classified call's id as `parent_id`, priced like any other call. A client's limits and its
  `_MODELS` do not apply to it.
- The newest user turn is classified, without the system prompt and tool results. An answer is
  reused for the same text for ten minutes, per instance.
- Proxy-mode calls that carry a conversation are classified too. Model lists, files,
  embeddings, speech and token counts are not.
- `PGW_LOG_BODIES=off` means never: unlike the full gateway, a flagged call does not have its
  bodies logged then.
- Not in the minimal gateway: routing and limits by intent, the try and evaluate panels, and
  alerts. A rule's `alert` has no effect here. The other settings of the full gateway's binding
  (what is classified, the confidence from which rules act, the cache time, the LLM's
  rationale and fields) keep their defaults.

## 13. Response cache

The full gateway's response cache ([plugin-concept.md](plugin-concept.md), section 15) runs in
the minimal gateway too, and is on by default. A call that was answered before is answered
again with the kept answer, without asking the provider. "Before" means the same model was sent
the same request: the same messages, tools and parameters, as the gateway shaped them. A stream
and a whole answer to the same request share one kept answer, and a kept answer is written in
the client's own dialect.

By default only calls with temperature 0 are cached. The temperature is read from the request
the provider is sent; a model that takes no temperature (most models that reason) is sent none,
and then the temperature the client asked for counts. Answers are kept in this instance's
memory, the oldest going first when it is full, and are gone when it stops.

| Variable | Default | Meaning |
|---|---|---|
| `PGW_CACHE_MODE` | `enforce` | `enforce`: answer from the cache. `shadow`: only log what would have been answered from it; every call reaches its provider. `off`: no cache. |
| `PGW_CACHE_CALLS` | `deterministic` | `deterministic`: calls with temperature 0. `all`: every call, whatever its temperature. |
| `PGW_CACHE_TTL_SECONDS` | 3600 | How long an answer is kept, 1 to 604800. |
| `PGW_CACHE_MAX_ANSWER_BYTES` | 1048576 | Larger answers are not kept, 1024 to 16777216. |
| `PGW_CACHE_MEMORY_MB` | 64 | The most the cache holds. |

Clients choose per call with the request header `x-gw-cache`, or `gateway.cache.response` in the
body on the native interface: `off` (neither answered from the cache nor kept), `refresh` (a
fresh answer, which is kept) or `on` (as configured). Every response of a call the cache looked
at says what it did in `x-gw-cache`:

| `x-gw-cache` | Meaning |
|---|---|
| `hit` | Answered from the cache. `Age` says how old the answer is, in seconds; `x-gw-attempts` is 0. |
| `miss` | Not there; the provider answered, and the answer is kept if it is worth keeping. |
| `would_hit` | Shadow mode found a kept answer; the provider answered all the same. |
| `refresh` | The client asked for a fresh answer. |
| `bypass` | Not a call the cache takes: not deterministic, or the client said `off`. |

- The call's log line carries `cache=` with the same value. A hit has `usage=cache`, no tokens
  and a cost of 0; `saved_cost` is what the kept answer would cost now.
- Kept: answers that ended normally (stop, length, tool calls), without a provider's own tools
  (a web search) and without encrypted reasoning. Not kept: refused or failed calls, cut
  streams, content-filtered answers.
- A hit takes nothing from a client's limits (`_RPM`, `_TPM`, `_CONCURRENCY`).
- Proxy mode, embeddings, speech and token counts are not cached. Requests that ask for several
  choices (`n`) or for log-probabilities are not cached either.
- Answers are kept per instance; the full gateway shares them between instances through Valkey.
- There is no clearing them: the minimal gateway has no management API. A restart empties the
  cache, and `x-gw-cache: refresh` replaces one answer.

## 14. Files and batches

Anthropic's Files and Message Batches and OpenAI's Files and Batches are served as in the full
gateway (its runbook, sections "Anthropic files and message batches" and "OpenAI files and
batches"): a batch's items are checked and limited like single calls, and each item is
accounted when the batch has ended, with its line in the log. The gateway keeps a record of
the files and batches made through it in `PGW_OBJECTS_DIR`, which `docker-compose.yml` sets to
the volume `gw-lite-objects`. Without the variable these endpoints answer 501, and proxy mode
still reaches the provider's own.

| Variable | Default | Meaning |
|---|---|---|
| `PGW_OBJECTS_DIR` | – (compose: `/var/lib/pacod-gw/objects`) | The directory of the record: an absolute path the gateway can write. |

- **One gateway per directory.** The gateway holds a lock on it while it runs; a second one on
  the same directory refuses to start. Several gateways behind a balancer each need their own
  directory, and a client must reach the gateway it made a file or batch with.
- **Batches are accounted where they were made.** Every 10 seconds the gateway asks about the
  open batches it made, at most once in 30 seconds per batch (at once when a client's call
  already saw it end), and writes each item's line in the log when the batch has ended.
- **All clients share one workspace:** each sees the others' files and batches.
- **The record** is an append-only file, `objects.jsonl`, read and rewritten at each start;
  deleted files are kept in it for 7 days. Back it up with the volume, or lose the knowledge
  of which batches are still to be accounted.

## 15. Limitations

- The configuration is read at start: a change needs a restart (a few seconds; running calls
  finish first).
- Limits are per instance and lost on restart. There are no daily or monthly budgets.
- No request history beyond the log, no reports, no metrics endpoint, no alerts.
- No price tracking: the prices in `.env` are what the log's costs are based on.
- The security trade-offs of sections 4 and 6: the bad-key block per source address, and
  provider keys that every client shares.
