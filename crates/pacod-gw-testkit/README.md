# pacod-gw-testkit

Test support for pacod-gw. Not shipped.

## Mock providers (`mock`)

One server emulates the OpenAI-compatible, Anthropic, Gemini (AI Studio and Vertex AI) and
Ollama APIs, and Google's token endpoint for service accounts. It records every request byte for
byte, answers deterministically, and can be scripted per request to delay, fail, break or stall.

### Library

```rust
use pacod_gw_testkit::mock::{Behavior, MockProvider};

let mock = MockProvider::spawn().await;            // 127.0.0.1, free port, stops on drop
let base = mock.base_url();                        // "http://127.0.0.1:54321"
mock.set_script(Behavior::parse_all(["fail=503x2,retry_after=1", "chunks=4"])?);
// ... make the gateway call the provider ...
let requests = mock.captured();                    // Vec<CapturedRequest>, oldest first
assert_eq!(&requests[0].body[..], expected_bytes);
```

| Item | Meaning |
| --- | --- |
| `MockProvider::spawn().await` | Bind `127.0.0.1:0` and serve in the background. Panics if it cannot bind. |
| `MockProvider::bind(addr).await` | Same on a given address; returns `io::Result`. |
| `base_url()`, `addr()` | `http://<addr>` without a trailing slash; the socket address. |
| `captured()` | Every provider request so far. Control requests (`/healthz`, `/__mock/*`) are not recorded. |
| `clear()` | Forget the captured requests. Leaves the script alone. |
| `set_script(behaviours)` | Replace the script queue (see below). `set_script([])` empties it. |
| `pending_script()` | Queue entries not used up yet. |
| `set_capture_limit(n)` | Keep only the `n` most recent requests; `0` captures nothing. Default: keep all. |
| `shutdown().await` | Stop and wait until the port is released. Dropping the handle stops without waiting. |

`CapturedRequest` has `method` (upper case), `path` (with the query string, as sent), `headers`
(`Vec<(String, String)>` in the order received; names lower-cased by the HTTP parser; a repeated
name has one entry per value, all at the position of its first occurrence) and `body` (`Bytes`,
exact). Helpers: `header(name)`, `json()`.

`Behavior` is the parsed form of an option list: `Behavior::parse("delay=200,chunks=8")`,
`"hang".parse()`, `Behavior::parse_all([...])`, or the public fields directly
(`Behavior { status: Some(500), ..Default::default() }`).

`count_tokens(text)`, `reply_text(user_text)`, `thinking_text(user_text)` and
`tool_arguments(user_text)` compute what the mock will answer, for assertions.

### Endpoints

| API | Endpoints |
| --- | --- |
| OpenAI-compatible | `POST /v1/chat/completions`, `POST /v1/responses` (also the emulated flavours), `GET /v1/models`, `GET /v1/models/{id}`, `POST /v1/embeddings`, `POST /v1/audio/speech`, `POST /v1/images/generations`, `POST /v1/images/edits`, `POST /v1/audio/transcriptions`, `POST /v1/audio/translations` (also the emulated flavours) |
| Anthropic | `POST /v1/messages`, `POST /v1/messages/count_tokens`, `GET /v1/models`, `GET /v1/models/{id}` |
| Gemini | `POST /v1beta/models/{model}:generateContent`, `POST /v1beta/models/{model}:streamGenerateContent[?alt=sse]`; for `imagen*` models `:predict`, and images from a `…-image` model asked for `responseModalities: IMAGE` |
| Ollama | `POST /api/chat`, `GET /api/tags`, `GET /api/version` |
| Rerank | `POST …/v2/rerank` (Cohere), `POST …/rerank` under a path naming `voyage` (Voyage), any other `POST …/rerank` (Jina, vLLM) |
| OpenAI Files and Batches | `POST`/`GET /v1/files`, `GET`/`DELETE /v1/files/{id}`, `GET /v1/files/{id}/content`, `POST`/`GET /v1/batches`, `GET /v1/batches/{id}`, `POST /v1/batches/{id}/cancel` |
| TypeSafe (System One) | `POST /v1/systemone` (also reached through the gateway's proxy mode); `GET /v1/models` as OpenAI's |
| OpenAI Realtime (WebSocket) | `GET /v1/realtime?model=…`, scripted: `session.created`; `session.update` → `session.updated`; `response.create` → `response.created`, a text delta and `response.done` with usage (audio and cached tokens included); `input_audio_buffer.commit` starts a response of its own, as voice activity detection does; a response whose `metadata.mock` is `"hold"` waits for `response.cancel` and ends `cancelled` |
| WebSocket echo | `GET /__ws/echo`: every message answered with itself, for proxy mode's byte relay |

Anything else is a 404 in the envelope of the API the path belongs to (OpenAI's by default).

- `GET /v1/models/{id}` knows every model id except those that start with `unknown`, which
  get a 404. The gateway's model probe uses it.
- `GET /v1/models` answers in Anthropic's shape when the request has an `anthropic-version` or
  `x-api-key` header, otherwise in OpenAI's.
- Anthropic endpoints answer 401 `authentication_error` without `x-api-key` and 400
  `invalid_request_error` without `anthropic-version`. The other APIs do not check credentials.
- Requests are validated lightly, with a 400 in the provider's envelope: the body must be a JSON
  object; OpenAI chat needs `model` and `messages`; Anthropic needs `model`, `max_tokens` and
  `messages`; Gemini needs `contents`; Ollama needs `model`.
- `POST /v1/responses` answers as chat does (an echo, the scripted tool, `thinking` as a
  reasoning summary), whole or streamed with the Responses API's events. Reasoning carries
  `encrypted_content` when the request includes `reasoning.encrypted_content`; a `web_search`
  tool adds one `web_search_call` item. Nothing is stored; `store` is echoed as sent.
- Images are a few bytes that start as a PNG does, derived from the request. gpt-image
  answers carry usage in tokens (1056 per 1024×1024 image); DALL·E answers a URL unless
  `response_format` is `b64_json`, and reports no usage; Imagen reports none either. A Gemini
  image model answers a line of text and one image, with usage by modality.
- Transcriptions are derived from the file's name (`Transcript of hello.wav`); the audio lasts
  one second per 16,000 bytes, at least one. Whisper models answer every format and report
  seconds (`usage.type: duration`, `verbose_json`'s `duration`, the cue of `srt` and `vtt`);
  `gpt-4o*-transcribe` models answer `json` and `text` only, report tokens, and stream.
- Rerank scores a document by the share of the query's words it contains. Cohere's shape
  bills one search unit per 100 documents; Jina's and Voyage's report tokens.
- OpenAI's batches run when first retrieved: every request of the input file is answered as
  its endpoint would answer it alone, into an output file; a `custom_id` starting with `error`
  goes to an error file. Every file's content can be read back.
- `POST /v1/systemone` answers each question in TypeSafe's shapes, by keyword: a choice is
  the first option whose words appear in the state (`legal_advice` matches "legal advice"),
  else `unknown`, with probability and confidence 0.9 when found and 0.4 otherwise; a `noul` is
  0.95 when the state names the question's id, 0.05 otherwise; a score is its lowest level, 0.
  Usage: the request's tokens as input, 10 as output. Credentials are not checked.
- Ollama streams unless the request says `"stream": false`. The others stream with
  `"stream": true` (Gemini: by endpoint).

### Deterministic answers

Response bodies depend only on the request path, query, body and scripted options.

- **Reply text**: `echo: <user text>`, where the user text is the text of the last `user`
  message (its text parts concatenated; for Anthropic including the text inside `tool_result`
  blocks; for Gemini the last content with role `user` or no role).
- **Thinking text** (option `thinking`): `thinking about: <user text>`.
- **Tool call** (option `tool=<name>`): arguments `{"input":"<user text>"}` instead of text.
- **Structured output** (OpenAI `response_format`): `json_object` gives `{"echo":"<user text>"}`;
  `json_schema` gives an instance of the schema (every property; strings are the user text,
  numbers `0`, booleans `false`, arrays `[]`, enums their first value). A schema named `intent`
  (the gateway's intent plugin) is answered by keyword: the first label whose words appear in
  the text (`legal_advice` matches "legal advice"), else `unknown`, with confidence 0.9 or 0.3;
  a flag is 0.95 when the text names its id and 0.05 otherwise.
- **Tokens**: `count_tokens(text)` is the number of characters divided by 4, rounded up.
  Input `I` is `count_tokens` of all prompt text concatenated without separators (system prompt
  and the text of every message). Output `O` is `count_tokens` of the reply text, or of the tool
  arguments. Reasoning `R` is `reasoning=<n>`, else `count_tokens(thinking text)` with
  `thinking`, else 0. `CR` and `CW` are `cache_read` and `cache_write` (default 0).

| API | Usage |
| --- | --- |
| OpenAI | `prompt_tokens = I + CR`, `prompt_tokens_details.cached_tokens = CR`, `completion_tokens = O + R`, `completion_tokens_details.reasoning_tokens = R`, `total_tokens` = their sum. `CW` is not reported. |
| Anthropic | `input_tokens = I`, `cache_read_input_tokens = CR`, `cache_creation_input_tokens = CW`, `output_tokens = O + R`. Streaming: `message_start` has the input counts and `output_tokens: 1`; `message_delta` has `{"output_tokens": O + R}` only. |
| Gemini | `promptTokenCount = I + CR`, `cachedContentTokenCount = CR`, `candidatesTokenCount = O`, `thoughtsTokenCount = R`, `totalTokenCount = I + CR + O + R`. Streaming: every chunk has `usageMetadata`; before the last it is `{promptTokenCount, totalTokenCount}` only. |
| Ollama | `prompt_eval_count = I`, `eval_count = O + R`, on the object with `done: true`. |

Example: system `be brief`, user `hello world` gives `I = 5` (19 characters), reply
`echo: hello world`, `O = 5` (17 characters).

- **Ids** are derived from a hash of the request: `chatcmpl-mock…`, `call_mock…`, `msg_mock…`,
  `toolu_mock…`, Gemini `responseId`. OpenAI `created` is always `1700000000`.
- **Embeddings**: 8 numbers per input (or `dimensions`), derived from the input text;
  `encoding_format: "base64"` is supported. **Speech**: 20 bytes or fewer that start like the
  requested container, with the matching `audio/*` content type (`audio/mpeg` by default).
- The reported model is the request's (Gemini: the one in the path) unless `model=` is set.

### Streams

A *content chunk* carries a piece of the model's output. By default each streamed string is cut
at word boundaries (`echo:`, ` hello`, ` world`); `chunks=<n>` cuts each string into `n` nearly
equal pieces instead (never more than one per character).

- **OpenAI** (`data: {json}\n\n`): a role chunk; with `thinking`, `delta.reasoning_content`
  chunks; `delta.content` chunks (for a tool call: the call's id and name, in the role chunk
  unless reasoning precedes it, then `delta.tool_calls[0].function.arguments` fragments); a chunk
  with empty `delta` and the `finish_reason`; then, only if the request has
  `stream_options.include_usage: true`, a chunk with `"choices": []` and `usage` (and every
  earlier chunk carries `"usage": null`); then `data: [DONE]`.
- **Anthropic** (`event: <type>\ndata: {json}\n\n`): `message_start`; per content block
  `content_block_start`, deltas, `content_block_stop`; `ping`; `message_delta`; `message_stop`.
  A `ping` also follows the first `content_block_start`. A thinking block (index 0, before the
  text or tool block) has `thinking_delta`s and then one `signature_delta`; a text block has
  `text_delta`s; a `tool_use` block has an empty `input_json_delta` and then the fragments.
- **Gemini** with `alt=sse` (`data: {json}\r\n\r\n`): one event per content chunk, the last
  with `finishReason` and the final `usageMetadata`. Thinking gives parts with `thought: true`
  first; the first part after them carries `thoughtSignature`. Without `alt=sse` the same
  objects form one JSON array.
- **Ollama** (`application/x-ndjson`): one line per content chunk with `done: false`
  (`message.content`, or `message.thinking`; a tool call is one line with
  `message.tool_calls`), then a line with `done: true`, `done_reason` and the counts.

Only `text_delta`/`thinking_delta`/non-empty `input_json_delta` events, OpenAI
content/reasoning/argument chunks, Gemini events and Ollama `done: false` lines are content
chunks; everything else is structure.

### Scripted behaviour: `x-mock`

A request header `x-mock` holds a comma-separated option list, for example
`x-mock: delay=200,chunk_delay=20,chunks=8`. Names are case-insensitive, `-` may be written for
`_`, and values cannot contain commas. An invalid list is answered with 400 and
`{"error":{"type":"mock_error","message":"mock: ..."}}`.

| Option | Effect |
| --- | --- |
| `delay=<ms>` | Wait before the response head (errors too). |
| `chunk_delay=<ms>` | Wait between two written stream chunks. |
| `chunks=<n>` | Cut each streamed string into `n` content chunks (`n` ≥ 1). |
| `split=<n>` | Re-chunk the stream's bytes into pieces of `n` bytes, so events span chunk boundaries. The concatenated bytes are unchanged. |
| `status=<code>` | Answer with this error status (400–599) in the provider's error envelope, message `mock: scripted error <code>`. |
| `fail=<code>x<n>` | Fail `n` requests with `<code>`, then answer one normally. See below. |
| `retry_after=<value>` | Send `retry-after: <value>` on responses failed by `status` or `fail`. |
| `reset_after=<n>` | Break the stream right after its `n`-th content chunk: the connection is dropped mid-body (HTTP/2: the stream is reset). `0` breaks right after the head. |
| `stall_after=<n>` | Stop sending right after the `n`-th content chunk and keep the connection open. |
| `hang` | Read the request and never answer. |
| `no_usage` | Leave usage out (OpenAI: no `usage`, no usage chunk; Anthropic: no `usage` objects; Gemini: no `usageMetadata`; Ollama: no counts). |
| `cache_read=<n>`, `cache_write=<n>`, `reasoning=<n>` | Set `CR`, `CW`, `R` above. |
| `thinking` | Emit reasoning before the answer: OpenAI `reasoning_content`, an Anthropic thinking block with a signature, Gemini thought parts, Ollama `message.thinking`. |
| `tool=<name>` | Answer with a call to this tool. Stop reason becomes `tool_calls` (OpenAI) or `tool_use` (Anthropic). |
| `stop=<reason>` | The finish reason, written as given: OpenAI `finish_reason`, Anthropic `stop_reason`, Gemini `finishReason`, Ollama `done_reason`. Defaults: `stop`, `end_turn`, `STOP`, `stop`. |
| `model=<id>` | Report this model as having served the request. |
| `request_id=<id>` | The request-id response header: `x-request-id` (OpenAI-compatible), `request-id` (Anthropic). Otherwise a new `req_<32 hex>` per response. |
| `tier=<name>` | The service tier the response reports (`service_tier`). |
| `code=<text>` | The business code of a scripted error, where the emulated provider has one: GLM `{"error":{"code"}}` (default `1302` for 429, `1214` for 400), Kimi `{"error":{"type"}}` (default `rate_limit_reached_error` for 429). |
| `reply=<base64url>` | The answer text instead of the echo, also when a JSON schema asks for structured output: a canned answer, for example a price extraction. |

`chunk_delay`, `chunks`, `split`, `reset_after` and `stall_after` only affect streamed
responses. A stream with fewer than `n` content chunks is not cut. With both `reset_after` and
`stall_after`, the smaller `n` decides. With `split`, `chunk_delay` applies between the pieces.

Error envelopes for `status` and `fail`:

- OpenAI: `{"error":{"message","type","param":null,"code"}}`; type `requests` and code
  `rate_limit_exceeded` for 429, `server_error` for 5xx, otherwise `invalid_request_error`
  (code `invalid_api_key` for 401).
- Anthropic: `{"type":"error","error":{"type","message"},"request_id"}`; type by status:
  401 `authentication_error`, 402 `billing_error`, 403 `permission_error`, 404
  `not_found_error`, 413 `request_too_large`, 429 `rate_limit_error`, 504 `timeout_error`, 529
  `overloaded_error`, other 5xx `api_error`, other 4xx `invalid_request_error`. `request_id`
  equals the `request-id` header, so pin it with `request_id=` for byte-stable error bodies.
- Gemini: `{"error":{"code","message","status"}}` with the gRPC status name
  (`RESOURCE_EXHAUSTED` for 429, `UNAVAILABLE` for 503, ...).
- Ollama: `{"error":"<message>"}`.

### Options in the path

The same options can lead the path: `/x-mock/<options>/<rest>`, for example
`/x-mock/delay=200,chunk_delay=20,chunks=40/v1/chat/completions`. The mock takes the options
and serves `/<rest>` as usual, after an `/emulate/<name>` prefix too. This exists for gateways
that forward no `x-mock` header: the full gateway's managed mode sends only a fixed set of
headers, but keeps a provider's base URL in front of every path it calls, so a provider whose
base URL is `http://gw-mock:9000/x-mock/delay=200/v1` gets the options on every call. Load tests
set their latency profile this way.

Path options count as an `x-mock` header that a real header overrides, option by option. An
invalid list is answered with 400 `mock_error`. Captured requests keep the full path.

### Script queue

For gateways that do not forward `x-mock`, queue behaviours in advance with `set_script` (or
`POST /__mock/script`). Each provider request takes the entry at the head of the queue; requests
after the queue is used up are answered normally. If a request also has an `x-mock` header, the
two are merged and the header wins for options set in both.

`fail=<code>x<n>`:

- In a queue entry, the entry stays at the head for `n + 1` requests: `n` failures, then one
  success to which the entry's other options apply. One entry therefore covers one gateway call
  with its retries.
- In an `x-mock` header, failures are counted per distinct request (method, path, query, body):
  the first `n` such requests fail, the next succeeds, and then the count starts over.
  `set_script` and `DELETE /__mock/script` reset these counts.

### Response headers

- OpenAI-compatible: `x-request-id`, `openai-organization`, `openai-processing-ms`,
  `openai-version`, and `x-ratelimit-{limit,remaining,reset}-{requests,tokens}`.
- Anthropic: `request-id`, `anthropic-organization-id`, and
  `anthropic-ratelimit-{requests,tokens,input-tokens,output-tokens}-{limit,remaining,reset}`.
- Gemini and Ollama: none of these, as in the real APIs; `request_id=` has no effect there.

All values are fixed except the generated request id. They are sent on errors as well.

### `gw-mock` and the control endpoints

```
gw-mock [--listen 0.0.0.0:9000] [--max-captured 10000]
```

`GW_MOCK_LISTEN` and `GW_MOCK_MAX_CAPTURED` set the same. The first line on stdout is
`gw-mock listening on http://<addr>` (useful with port `0`); logs go to stderr, in
`PGW_LOG_FORMAT` (`json`, the default, or `text`) with `SERVICE_NAME` (default `gw-mock`) as their
`service` (pacod-demo/docs/LOGGING.md), and `RUST_LOG=debug` logs every request. `--max-captured 0` turns capture off.

The control endpoints are served by the library mock as well. They are never captured and never
use up the script.

| Endpoint | Effect |
| --- | --- |
| `GET /healthz` | `200 {"status":"ok"}` |
| `GET /__mock/requests` | The captured requests, oldest first: `[{"method","path","headers":[["name","value"],...],"body_base64"}]`. This is the JSON form of `CapturedRequest`. |
| `DELETE /__mock/requests` | Forget them. `204`. |
| `POST /__mock/script` | Body: a JSON array of option lists, one per request, e.g. `["fail=503x2,retry_after=1","chunks=4"]`. Replaces the queue. `200 {"queued": 2}`, or `400` if a list is invalid (the queue is then unchanged). |
| `DELETE /__mock/script` | Empty the queue. `204`. |
| `GET /__mock/prices.json` | A fixed price list in the gateway's feed format, for the price tracker: `tracked-steady` (2.20 / 10.00) and `tracked-jump` (4.00 / 20.00, cache read 0.40), USD per million tokens. |
| `GET /__mock/pricing.html` | The same figures as an HTML pricing page. |
| `GET /__mock/google` | `{"minted": n}`: how many access tokens the emulated Google token endpoint issued. |

### Provider emulations: `/emulate/<provider>/…`

Under a path prefix the OpenAI-compatible endpoint answers as one provider does. The prefix is
stripped and the rest of the path matched by its end (`…/chat/completions`, `…/models`,
`…/models/{id}`), so a provider is set up with its real base path below the prefix. Shapes come
from the recorded fixtures (`fixtures/`) where they exist and from the provider's documentation
otherwise. `src/mock/flavour.rs` says which, knob by knob.

| Prefix | Base URL to configure | What differs |
| --- | --- | --- |
| `/emulate/deepseek` | `{mock}/emulate/deepseek` | Thinks unless `thinking.type` is `disabled` or `reasoning_effort` is `none`. Usage adds `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`; a stream carries it on the finish chunk, asked or not, and starts with a `: keep-alive` comment. 400 when `tools` are present and an earlier assistant message has no `reasoning_content` (documented; the real API was more lenient when recorded), and for `tool_choice` `required` or named while thinking. |
| `/emulate/glm` | `{mock}/emulate/glm/api/paas/v4` | Thinks unless `thinking.type` is `disabled`. A stream always carries usage on the finish chunk, asked or not. Errors `{"error":{"code":"<string>","message"}}`. |
| `/emulate/kimi` | `{mock}/emulate/kimi/v1` | Always thinks. Usage has a top-level `cached_tokens` and `prompt_tokens_details.cache_write_tokens` (`prompt_tokens` includes both). A stream puts a short usage in `choices[0].usage` of the finish chunk, and the full one in a chunk of its own when asked. 400 for a `temperature`, `top_p` or penalty other than the fixed value, for `n` > 1, and for a named `tool_choice`. Errors `{"error":{"type","message"}}`. |
| `/emulate/gemini-openai` | `{mock}/emulate/gemini-openai/v1beta/openai` | A tool call, or a text answer as a whole, carries `extra_content.google.thought_signature` (a stream: on its last chunk); 400 when the first tool call of a step in the current turn comes back without one, or when the request ends with an assistant turn. Reasoning tokens are in `total_tokens` only, not in `completion_tokens`. Errors are wrapped in an array: `[{"error":{"code","message","status"}}]`. Model ids start with `models/`. |
| `/emulate/xai` | `{mock}/emulate/xai/v1` | xAI's Chat Completions: reasoning tokens in `completion_tokens_details.reasoning_tokens`, outside `completion_tokens` and inside `total_tokens`; no reasoning text unless scripted. The reasoning models (`grok-4…`, `grok-code…`, `…mini`, not `…non-reasoning`) refuse `stop` and the penalties with 400, and all but the mini models `reasoning_effort` too. Errors `{"code","error"}`. From the documentation and LiteLLM; nothing recorded. |
| `/emulate/ollama` | `{mock}/emulate/ollama/v1` | Reasoning in `reasoning`, only when `reasoning_effort` is given and not `none`. Usage without detail fields. |
| `/emulate/gemini` | `{mock}/emulate/gemini/v1beta` | The native API with Gemini 3's checks: every function call answered carries a `thoughtSignature`; 400 when the first function call of a model step in the current turn has none, or when `contents` ends with a model turn. Also `:countTokens` (`contents` or a whole `generateContentRequest`), `:embedContent` and `:batchEmbedContents` (at most 100 requests, each naming its model), without usage, as recorded. |
| `/emulate/vertex` | `{mock}/emulate/vertex/v1/projects/{p}/locations/{l}/publishers/google` (express: `{mock}/emulate/vertex/v1/publishers/google`) | Gemini on Vertex AI: the native API with the same checks, `usageMetadata.trafficType`, and `:predict` for text embedding models; `GET …/v1beta1/publishers/google/models` lists models. Only an access token the token endpoint below issued is accepted, or an API key in `x-goog-api-key` on the express path; anything else is 401 `UNAUTHENTICATED`. Shapes from Vertex's reference until `fixtures/vertex/` is recorded. |
| `/emulate/google-oauth/token` | – | Google's token endpoint for the JWT-bearer grant of a service account. Checks the assertion's shape (RS256, issuer, cloud-platform scope, at most an hour), not its signature, and issues a token; `?expires_in=<seconds>` on the URL shortens its life. |

The `x-mock` options apply under every prefix. Anything else under a prefix is a 404 in that
provider's envelope.

## The minimal gateway's smoke test (`lite`, `gw-smoke lite`)

The minimal gateway (`docs/lite.md`) has no management API, so `gw-smoke lite` sets nothing
up: it calls the models, pool and providers its options name, which default to the fixed
configuration of `deploy/lite/docker-compose.dev.yml` (gateway 18280, mock 19280). An empty
option skips its checks. It prints every call's request id (`request-id <id> <check>`), which
`scripts/smoke-lite.sh` and `scripts/lite-live.sh` then look up in the gateway's log.

## Recorded fixtures (`record`, `fixtures/`)

`gw-smoke record` (or `make record-fixtures`) calls DeepSeek, GLM, Gemini's compatible endpoint
and Gemini's native API directly with the keys in the environment and writes what they answered
to `fixtures/<provider>/<case>.json`, scrubbed. `fixtures/README.md` describes the cases, the
format and what is scrubbed.

### Where the mock differs from the real APIs

- Anthropic also accepts OAuth tokens in `Authorization: Bearer`; the mock requires `x-api-key`.
- Anthropic `message_delta.usage` holds only `output_tokens` (the documented minimum); the real
  API may repeat the input counts. `stop_details` for `refusal` and the `capabilities` tree of
  `GET /v1/models` follow the public reference but are abbreviated.
- Gemini always reports all four usage counts; the real API omits zero-valued ones. Outside
  `/emulate/gemini`, a `thoughtSignature` is only sent with `thinking`.
- `reasoning_content` is the de-facto field of OpenAI-compatible servers, not part of OpenAI's
  own API. OpenAI error `type`/`code` values per status are approximate.
- Header names are captured lower-cased, because the HTTP parser normalises them.
