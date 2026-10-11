# Provider fixtures

What providers answered to a fixed set of small requests, one directory per provider and one
JSON file per exchange (concept section 8.6). The dialect readers, the usage extractor and the
mock's emulations are tested against these files, so that what a provider's documentation leaves
open is settled by what the provider sent.

## Recording

```sh
make record-fixtures                          # every provider whose key is in .env
make record-fixtures RECORD_ARGS="--provider glm"
make record-fixtures RECORD_ARGS="--provider gemini --case embed"   # one case only
```

`scripts/record-fixtures.sh` reads `GEMINI_API_KEY`, `DEEPSEEK_API_KEY` and `ZAI_API_KEY` from
`.env` and runs `gw-smoke record`, which calls the providers directly (not through the gateway).
A run costs a fraction of a cent per provider. Directories: `deepseek`, `glm`, `gemini-openai`
(Gemini's OpenAI-compatible endpoint), `gemini` (the native API) and `vertex` (Gemini on Vertex
AI).

Vertex is recorded with a service account: `VERTEX_PROJECT`, `VERTEX_LOCATION` and
`VERTEX_CREDENTIALS_FILE` (the path of the account's JSON key file; keep it outside the
repository). The recorder signs a token request with the key and calls Vertex with the token.
Paths are stored below `…/v1/projects/{project}/locations/{location}/publishers/google`; a case
with `"root": true` (the project-less model list) stores its path below the host. Vertex's
`tools-2-foreign-signature` sends the model turn of `gemini/tools-1.json`, so record `gemini`
first.

`--case` records single cases without touching the others; cases that need an earlier answer
(the second turn of a tool loop) are skipped with it.

## Format

```json
{
  "source": "recorded",
  "recorded": "2026-10-11",
  "provider": "deepseek",
  "case": "tools-2",
  "request": { "method": "POST", "path": "/chat/completions", "body": { } },
  "response": { "status": 200, "headers": { "content-type": "application/json" }, "body": { } }
}
```

- `response.body` holds a whole JSON answer; `response.text` holds anything else (a stream) as the
  exact text, framing included.
- `path` is below the provider's base URL (for example `https://api.deepseek.com`).
- A fixture written from documentation instead of a recording has `"source": "docs"` and a
  `"doc_url"`. Every such file is replaced by a recording once the provider's key is available.
  None is left: every file here was recorded on 2026-10-11. Kimi and Ollama have no fixtures;
  the mock's emulations of them follow their documentation (`src/mock/flavour.rs`).

## What is scrubbed

The files are committed, so the recorder removes what identifies an account or a call:

- response headers are kept only from an allow-list: `content-type`, `retry-after`, and
  `x-ratelimit-*` by name, with the value `fx`;
- the values of `id`, `responseId`, `system_fingerprint`, `tool_call_id`, `request_id` and
  `requestId` become `fx-1`, `fx-2`, …, the same placeholder for the same value across a
  provider's run (a tool call's id in one answer still matches the id sent back in the next
  request);
- `projects/<number>` in Google's error details becomes `projects/0`; a Vertex project id becomes
  `fx-project` and the service account's address `fx-service-account`, wherever they appear;
- a file that would contain one of the provider keys in the environment, the access token or the
  service account's key id is not written.

`tests/fixtures.rs` checks every committed file for the header allow-list and for strings that
look like keys, private keys, service-account addresses or Google access tokens.

`testdata/test-service-account.json` (beside this directory) is a service-account key made for
the tests only: it belongs to no Google project and opens nothing.
