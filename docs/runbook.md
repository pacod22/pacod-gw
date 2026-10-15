# pacod-gw runbook

This document is for the person who installs and runs pacod-gw. It assumes Docker and a
shell, and no knowledge of the project. The design is described in
[ai-gateway-concept.md](ai-gateway-concept.md); you do not need it to operate the stack.

**State of this document.** The steps of section 4 were followed once from a clean clone of
the repository, on one machine (Docker Desktop on macOS, 2026-10-05), with `PGW_TLS=internal`
and the names `api.localhost` and `console.localhost`: the three images build, the stack
starts with every service healthy, the API answers through Caddy with TLS (a streamed call
included), the console works in a browser, and calls to OpenAI and Anthropic succeed. The
backup was then taken with `backup.sh`, all volumes were removed, and `restore.sh` brought
the stack back: a gateway key created before the backup was accepted and the provider
credential could be decrypted.

The layout with several instances (section 9) was run on the same machine on 2026-10-07:
the scale layout with `core` as the management instance and two gateways, plus a third
gateway from `deploy/node/` in a compose project of its own that shared no Docker network
with the stack and reached PostgreSQL, the analytics service and the collector only through
their published ports, as another machine would. Caddy balanced over the three. The smoke
test (mock and live), the tester's mock catalog and 40 000 calls of load went through the
balancer without a failed call; the calls and their usage events spread evenly over the
three gateways; a revoked key was refused by every gateway within a second; a gateway
replaced under load cost no call, a killed one only the calls it was serving; a second
process with a running instance's id was refused; a killed gateway was reported as stopped
while the others served on; Prometheus scraped all four instances under their ids. The
same layout ran once more with `docker-compose.hub.yml` and the `backend` network internal,
as in production: that gateway then reached PostgreSQL, the analytics service and the
collector only through the hub's ports, its usage events arrived, and Prometheus scraped it.

Not exercised: certificates from Let's Encrypt (that needs real DNS names), a Linux host,
gateways on physically separate machines and TLS between them, an upgrade from one release
to the next, and the Helm chart on a cluster. `make deploy-check` validates the configuration of every
component with its own tool. Where a step could not be exercised, the text says so.

Signing in through pacod-id (section 16) was exercised on the development stack only, on
2026-10-13: `PGW_AUTH=oidc make dev-up` came up healthy, the smoke test passed with
`SMOKE_AUTH=oidc`, the console signed in through pacod-id in a browser, and the time to
revoke a session was measured; a backup with pacod-id's database was made and restored, and
people signed in afterwards. The move of a gateway's people to pacod-id was run end to end
with the command-line tools (`identity export`, `pacod-id import gw`, `identity rollback`,
`identity finalize`) against a core and a pacod-id started by hand, and the test
`crates/pacod-gw-core/tests/oidc_migration.rs` covers it with pacod-id in process. The
production stack with pacod-id, an upgrade to this release, and the compose commands of
section 16 for the move were not exercised.

Contents:

1. [What runs](#1-what-runs)
2. [Ports](#2-ports)
3. [Secrets](#3-secrets)
4. [First deployment](#4-first-deployment)
5. [Health checks](#5-health-checks)
6. [Upgrade](#6-upgrade)
7. [Backup and restore](#7-backup-and-restore)
8. [Rotating keys](#8-rotating-keys)
9. [Scaling](#9-scaling)
10. [When something fails](#10-when-something-fails)
11. [Development stack](#11-development-stack)
12. [Audit mode, the price job, moving a configuration](#12-audit-mode-the-price-job-moving-a-configuration)
13. [Account emails](#13-account-emails)
14. [Members and invitations](#14-members-and-invitations)
15. [Running with make](#15-running-with-make)
16. [Signing in through pacod-id](#16-signing-in-through-pacod-id)

All commands are run from the repository root. Section 15 lists the `make` targets that
wrap the most common of them. `dc` stands for:

```sh
dc() { docker compose -f deploy/docker-compose.yml "$@"; }
```

## 1. What runs

pacod-gw is a gateway between applications and LLM providers. Applications send their
requests to the gateway with a gateway key. The gateway forwards them to the provider with
the provider's key, counts tokens and cost, and enforces limits.

The compose stack (`deploy/docker-compose.yml`) starts these services:

| Service | What it is | Keeps data in |
|---|---|---|
| `core` | The gateway: data plane, management API, metrics. | volume `spool` (events waiting to be shipped) |
| `analytics` | Stores usage events, answers reports, runs batch jobs. | nothing of its own |
| `ui` | The admin console. | nothing |
| `tester` | Runs test cases against the gateway for the console's Testing page (platform admins only). | volume `tester-data` (run history, uploaded suites) |
| `id` | pacod-id: signs people in with `PGW_AUTH=oidc` and holds their accounts and roles (section 16). It runs in either mode. | its database `pacod_id` in `postgres` |
| `id-db` | Runs once at every start and creates pacod-id's PostgreSQL role and database, or sets the role's password. | nothing |
| `postgres` | Configuration, encrypted provider credentials, audit log, limit counters. | volume `postgres-data` |
| `clickhouse` | Usage events, audit-mode records, rollups. | volume `clickhouse-data` |
| `caddy` | TLS and the only entry from outside. | volumes `caddy-data`, `caddy-config` (certificates) |
| `otel-collector` | Receives traces from the core. | nothing |
| `prometheus` | Scrapes the core's metrics, evaluates the alert rules. | volume `prometheus-data` |
| `alertmanager` | Sends the alerts by email and to a webhook (section 10). | volume `alertmanager-data` |
| `grafana` | Dashboards. | volume `grafana-data` |
| `preflight` | Runs once at every start and checks that the secrets are set. | nothing |

With `docker-compose.load.yml` (load tests from the console, off by default):

| Service | What it is | Keeps data in |
|---|---|---|
| `load-agent` | Makes the load of the console's load tests, for the tester ([testing.md](testing.md), section 9). | nothing |
| `load-mock` | The mock provider behind load tests against the mock. Never a provider of real traffic. | nothing |

Requests keep flowing when PostgreSQL, ClickHouse or the analytics service is down. The
core works from the last configuration it loaded and collects events in its spool.

With the scale layout, `core` serves only the management API and the services `gateway-1`,
`gateway-2` serve the data plane, each with a spool volume of its own; gateways on other
machines run `deploy/node/` (section 9). Load agents on other machines run
`deploy/load-agent/` (section 9).

The minimal gateway (`deploy/lite/`, section 17) is not part of this stack: one container,
`gateway` in the compose project `pacod-gw-lite`, that keeps no data and needs none of the
services above.

## 2. Ports

Only Caddy publishes ports on the host.

| Host port | Variable | Serves |
|---|---|---|
| 80 | `PGW_HTTP_PORT` | Redirect to HTTPS, and the certificate challenge. |
| 443 (TCP and UDP) | `PGW_HTTPS_PORT` | The API host, the console host and pacod-id's host (`PGW_ID_HOST`). |

Inside the compose network:

| Service | Port | Serves | Reachable from outside |
|---|---|---|---|
| `core` | 8080 | Data plane: `/v1`, `/gw/v1`, `/proxy`, `/healthz`, `/readyz` | Yes, through Caddy on the API host. |
| `core` | 8081 | Management API: `/api/v1` | No. The console calls it inside the network. |
| `core` | 9100 | `/metrics` | No. |
| `analytics` | 8090 | Event ingest and reports | No. |
| `ui` | 3000 | Admin console | Yes, through Caddy on the console host. |
| `tester` | 8095 | The tester's API, for the console | No. |
| `load-agent` | 8096 | A load agent's API, for the tester (with `docker-compose.load.yml`) | No. |
| `load-mock` | 9000 | The mock provider of load tests, for the gateway (with `docker-compose.load.yml`) | No. |
| `id` | 8070 | pacod-id's sign-in pages and OpenID Connect endpoints | Yes, through Caddy on `PGW_ID_HOST`. |
| `id` | 8071 | pacod-id's internal API: tokens, the feed of roles, its admin API | No. The core, the console and the tester call it inside the network. |
| `id` | 9170 | pacod-id's `/metrics`, `/healthz` and `/readyz` | No. |
| `postgres` | 5432 | PostgreSQL | No. |
| `clickhouse` | 8123 | ClickHouse HTTP interface | No. |
| `otel-collector` | 4318, 4317, 13133, 8888 | OTLP over HTTP, OTLP over gRPC, health, own metrics | No. |
| `prometheus` | 9090 | Prometheus | No. |
| `alertmanager` | 9093 | Alertmanager | No. |
| `grafana` | 3000 | Grafana | Only for allow-listed addresses, at `/grafana/` on the console host. |
| `caddy` | 2019 | Caddy's admin endpoint, loopback only | No. |

PostgreSQL and ClickHouse sit on a network without a route to the host (`backend`).

Caddy also listens on 8088 inside the stack: the balanced data plane without TLS, for the
tester of the scale layout and the load agent. It is not published.

With `docker-compose.hub.yml` (gateways on other machines, section 9) the central host
publishes on its private address `PGW_CLUSTER_BIND`:

| Host port | Variable | Serves |
|---|---|---|
| 5432 | `PGW_CLUSTER_POSTGRES_PORT` | PostgreSQL, for the gateways. |
| 8090 | `PGW_CLUSTER_ANALYTICS_PORT` | The analytics service: usage events from the gateways. |
| 4318 | `PGW_CLUSTER_OTLP_PORT` | The trace collector. |

A gateway on another machine (`deploy/node/`) publishes on its private address
`PGW_NODE_BIND` the data plane (`PGW_NODE_DATA_PORT`, 8080) for the central Caddy and its
metrics (`PGW_NODE_METRICS_PORT`, 9100) for the central Prometheus.

A load agent on another machine (`deploy/load-agent/`) publishes its API on its private
address `LOAD_AGENT_BIND`, port `LOAD_AGENT_PORT` (8096), for the central tester.

The minimal gateway (section 17) publishes its data plane, without TLS, on
`PGW_LITE_BIND:PGW_LITE_PORT`, by default 127.0.0.1:14080. It uses none of the ports above, so
it can run on the same host.

## 3. Secrets

All settings are in `deploy/.env`. The file is git-ignored. `deploy/.env.example` lists
every variable with a comment.

| Variable | What it protects | Generate with | If it is lost or changed |
|---|---|---|---|
| `PGW_KEK` | Key-encryption key. Provider credentials in PostgreSQL are encrypted under it. | `openssl rand -base64 32` | **The stored provider credentials cannot be read. They must all be entered again.** |
| `PGW_KEY_PEPPER` | Gateway keys and admin tokens are stored as an HMAC under it. | `openssl rand -base64 32` | Every gateway key and admin token stops working. |
| `PGW_INTERNAL_TOKEN` | The calls between core and analytics. | `openssl rand -hex 32` | Can be changed at any time (section 8). |
| `POSTGRES_PASSWORD` | The PostgreSQL user `pacod_gw`. | `openssl rand -hex 24` | Can be changed (section 8). |
| `CLICKHOUSE_PASSWORD` | The ClickHouse user `pacod_gw`. | `openssl rand -hex 24` | Can be changed (section 8). |
| `PGW_ADMIN_PASSWORD` | The first administrator of the console. | `openssl rand -base64 18` | Used only once; change it in the console. |
| `GRAFANA_ADMIN_PASSWORD` | The Grafana administrator. | `openssl rand -base64 18` | Used only when Grafana first starts. |
| `PGW_SMTP_PASSWORD` | The SMTP account that sends account emails (section 13). | From the mail provider | Account emails fail until it is corrected; nothing else is affected. |

pacod-id's secrets (`PID_DB_PASSWORD`, `PID_KEK`, `PID_TOKEN_PEPPER`, the three client
secrets and the console's `PGW_UI_SESSION_SECRET`) are listed in section 16. Preflight
requires them whichever `PGW_AUTH` says, because pacod-id always runs.

Rules:

- `POSTGRES_PASSWORD` and `CLICKHOUSE_PASSWORD` may contain only letters, digits and
  `. _ ~ -`. The PostgreSQL password becomes part of a URL. `openssl rand -hex` produces such
  values.
- Provider API keys are not in this file. They are entered in the console and stored
  encrypted in PostgreSQL.
- The compose files pass these secrets to the containers as variables from `deploy/.env`.
  Outside compose, the core can also read them from files, where secrets are mounted as
  files: `PGW_KEK_FILE`, `PGW_KEK_OLD_FILE`, `PGW_KEY_PEPPER_FILE`,
  `PGW_INTERNAL_TOKEN_FILE`, `DATABASE_URL_FILE`, `PGW_ADMIN_PASSWORD_FILE` and
  `PGW_SMTP_PASSWORD_FILE` each name one file. Or `PGW_SECRETS_DIR` names a directory with
  one file per variable, named like it, where a missing file means unset; the Helm chart
  does this with `secrets.mount: true`. Setting a variable and its `_FILE` both is refused
  at start. `PGW_KEK_OLD` in a file may have one `id=key` per line.

### Storing the key-encryption key

`PGW_KEK` is the one secret that cannot be recreated. Before the first start:

1. Copy the value of `PGW_KEK` and of `PGW_KEK_ID` into a password manager or a secret
   store that is **not on this host** and **not where the backups go**.
2. Do the same for `PGW_KEY_PEPPER`, and for pacod-id's `PID_KEK`, `PID_KEK_ID` and
   `PID_TOKEN_PEPPER`.

The reason for the separation: a database backup together with the key gives access to
every provider credential. Apart, neither is of use to someone who obtains it. The backup
script does not copy `deploy/.env` and refuses to run until you confirm that the key is
stored elsewhere (section 7).

## 4. First deployment

You need a Linux host with Docker Engine and the compose plugin, 8 GB of memory, and three
DNS names that point to it: one for the API, one for the console and one for pacod-id, where
people sign in with `PGW_AUTH=oidc` (section 16). Caddy serves and obtains a certificate for
all three, whichever mode the console uses.

**Step 1. Get the code.**

```sh
git clone <repository> pacod-gw && cd pacod-gw
```

**Step 2. Get the images.** Either build them on the host:

```sh
docker build -f deploy/docker/rust.Dockerfile --target core      -t pacod-gw-core:0.1.0 .
docker build -f deploy/docker/rust.Dockerfile --target analytics -t pacod-gw-analytics:0.1.0 .
docker build -f deploy/docker/ui.Dockerfile                      -t pacod-gw-ui:0.1.0 .
docker build -f vendor/pacod-id/deploy/docker/pacod-id.Dockerfile --target pacod-id \
  -t pacod-id:0.1.0 vendor/pacod-id
```

(`make images` runs these four commands and builds the tester's image as well), or, if
your organisation publishes them to a registry, set `PGW_REGISTRY` in step 3 (with the
trailing slash, for example `registry.example.com/platform/`) and Docker pulls them. The tag
must equal `PGW_VERSION`.

**Step 3. Create the settings file.**

```sh
cp deploy/.env.example deploy/.env
chmod 600 deploy/.env
```

Generate the secrets and paste each line over the empty one in `deploy/.env`:

```sh
printf 'POSTGRES_PASSWORD=%s\n'      "$(openssl rand -hex 24)"
printf 'CLICKHOUSE_PASSWORD=%s\n'    "$(openssl rand -hex 24)"
printf 'PGW_KEK=%s\n'                "$(openssl rand -base64 32)"
printf 'PGW_KEY_PEPPER=%s\n'         "$(openssl rand -base64 32)"
printf 'PGW_INTERNAL_TOKEN=%s\n'     "$(openssl rand -hex 32)"
printf 'PGW_ADMIN_PASSWORD=%s\n'     "$(openssl rand -base64 18)"
printf 'GRAFANA_ADMIN_PASSWORD=%s\n' "$(openssl rand -base64 18)"
printf 'PID_DB_PASSWORD=%s\n'         "$(openssl rand -hex 24)"
printf 'PID_KEK=%s\n'                 "$(openssl rand -base64 32)"
printf 'PID_TOKEN_PEPPER=%s\n'        "$(openssl rand -hex 32)"
printf 'PGW_ID_CLIENT_SECRET=%s\n'    "$(openssl rand -hex 32)"
printf 'PGW_UI_CLIENT_SECRET=%s\n'    "$(openssl rand -hex 32)"
printf 'TESTER_ID_CLIENT_SECRET=%s\n' "$(openssl rand -hex 32)"
printf 'PGW_UI_SESSION_SECRET=%s\n'   "$(openssl rand -hex 32)"
```

The last seven belong to pacod-id and the console's sign-in through it (section 16).
Preflight requires them even while the console keeps its own login (`PGW_AUTH=local`, the
default).

Then set, in the same file:

| Variable | Set it to |
|---|---|
| `PGW_API_HOST` | The DNS name of the API, for example `api.example.com`. |
| `PGW_CONSOLE_HOST` | The DNS name of the console, for example `console.example.com`. |
| `PGW_ID_HOST` | The DNS name of pacod-id, for example `id.example.com`. |
| `PGW_TLS` | Your e-mail address. Caddy then obtains certificates from Let's Encrypt. Leave `internal` only for a test without real DNS names. |
| `PGW_ADMIN_EMAIL` | The e-mail address you will log in with. |
| `PGW_KEK_ID` | A name for the key, for example the month: `2026-10`. |
| `PID_KEK_ID` | A name for pacod-id's key, for example `2026-10`. |
| `GRAFANA_ROOT_URL` | `https://<console host>/grafana/` |

To have people sign in through pacod-id from the start, see section 16, "A new installation",
before step 6.

**Step 4. Store `PGW_KEK`, `PGW_KEY_PEPPER`, `PID_KEK` and `PID_TOKEN_PEPPER` somewhere
else** (section 3). Do it now.

**Step 5. Check the file.**

```sh
dc config --quiet && echo "the compose file is valid"
```

**Step 6. Start.**

```sh
dc up -d --wait
```

`--wait` returns when every service is healthy. The first start takes a few minutes:
PostgreSQL and ClickHouse initialise their volumes, the core and the analytics service
create their schemas, Grafana creates its database, and Caddy obtains certificates.

If the command stops with `service "preflight" didn't complete successfully`, a secret is
missing or malformed. `dc logs preflight` names it.

**Step 7. Verify.**

```sh
dc ps                                         # every service "running (healthy)"; preflight "exited (0)"
curl -s https://api.example.com/readyz        # {"status":"ready","config_version":...}
```

Open `https://console.example.com/`, log in with `PGW_ADMIN_EMAIL` and
`PGW_ADMIN_PASSWORD`, and change the password. With `PGW_AUTH=oidc` you sign in at pacod-id
instead, as section 16 describes.

With `PGW_TLS=internal`, clients do not trust the certificate. Either pass the root
certificate to them (`dc exec caddy cat /data/caddy/pki/authorities/local/root.crt`) or use
`curl -k` for a test.

**Step 8. Set up the gateway in the console.** Add a provider from its preset and give it
its API key (see "Setting up each provider" at the end of this section), activate a model,
create an organisation and a workspace, and create a gateway key. Applications then call
`https://api.example.com/v1` with that key.

**Step 9. Schedule the backup** (section 7).

**Optional: Grafana.** Grafana is served at `https://<console host>/grafana/`, and only to
the addresses in `PGW_ADMIN_ALLOW_CIDRS`. The default admits nobody. Set it to your office
or VPN range, for example `PGW_ADMIN_ALLOW_CIDRS=203.0.113.0/24`, and run
`dc up -d caddy`. Log in as `GRAFANA_ADMIN_USER`. The four dashboards are in the folder
`pacod-gw`. Check in `dc logs caddy` which client address Caddy sees before you rely on the
list: on some Docker setups (IPv6 clients without IPv6 container networking) every client
appears with a private address. Set `PGW_GRAFANA_URL` to the same address
(`ui.grafanaUrl` in the Helm chart) and the console's Monitoring page links to Grafana;
unset, the link is hidden.

**Optional: the management API from outside.** Scripts and CI may need it. Uncomment the
block marked for it in `deploy/caddy/Caddyfile`. It is then served at
`https://<API host>/api/v1/` for the addresses in `PGW_ADMIN_ALLOW_CIDRS`. Without that
block, call it from inside the network:

```sh
docker run --rm --network pacod-gw_backend curlimages/curl -s http://core:8081/readyz
```

### Setting up each provider

Platform, Providers, Add, then choose a preset: it fills in the family, the base URL, how the
key is sent, the managed-mode API and the provider's quirks (what differs from OpenAI's API).
Then add the key under the provider's credentials, and add models: run discovery (section 12,
Model discovery) or add a model by hand. A new model gets the capability preset its id
suggests; check it on the draft, give the model a price (or mark it free), and activate it.
Activation probes the model with the shared credential.

| Provider | Presets | What to know |
| --- | --- | --- |
| OpenAI | `openai` | – |
| Anthropic | `anthropic` | – |
| Google Gemini | `gemini` (Gemini's own API), `gemini-openai` (its OpenAI-compatible endpoint) | Use `gemini` unless something needs the compatible endpoint: it returns thought text and takes Gemini's own tools. Both can exist side by side under different names. The key is a Gemini API key from Google AI Studio. Google's SDKs reach every model through the Gemini interface (below), or Gemini itself through proxy mode: base URL `https://<API host>/proxy/<provider name>`. On the shared credential, Gemini's files, context caches and file search stores are refused; a workspace that needs them gets a credential of its own. Capability presets `gemini-3-flash`, `gemini-3-flash-lite`, `gemini-3-pro`, `gemini-2-5`, and `embedding` for `gemini-embedding-001`. |
| Gemini on Vertex AI | `vertex` (a service account), `vertex-express` (an API key) | For `vertex`, create a service account in the Google Cloud project, give it the role Vertex AI User (`roles/aiplatform.user`) on that project and nothing else, and create a JSON key for it. In the preset's base URL replace `YOUR-PROJECT` with the project id and the location (twice) with the region the models run in. Add the key file as the credential: the credential dialog takes the file, or paste its content. The gateway stores it encrypted, shows only the account and the key id, and gets access tokens with it from Google's token endpoint, which the gateways must reach (`oauth2.googleapis.com`). The token opens the whole project: proxy mode allows only model calls of that project and location, and on the shared credential refuses Cloud Storage objects, the project's data stores and model routing; IAM is what limits the key otherwise, so keep the service account to this one role and project. `vertex-express` takes an express-mode API key instead. Vertex AI has no model list: add models by hand (the ids are Gemini's, for example `gemini-3.8-flash`), with the same capability presets as Gemini. Rotate a key file like any key (section 8): add the new one, then delete the old key in Google Cloud. |
| DeepSeek | `deepseek` | Thinks by default; the effort `none` switches it off. The gateway sends earlier reasoning back on turns with tools. Off-peak rates apply all weekend and on weekdays outside 01:00–04:00 and 06:00–10:00 UTC: LiteLLM's list (key prefix `deepseek`) carries them as the variant `off_peak` with its windows, or enter them by hand (`variants.off_peak` and `windows`). They apply all day on Chinese public holidays too: the preset links the provider to the holiday calendar `cn` (section 12, Holiday calendars); a DeepSeek provider created before needs `holidays` set to `cn`. Capability presets `deepseek`, `deepseek-vision`. |
| GLM | `glm` (`https://api.z.ai/api/paas/v4`), `glm-cn` (`https://open.bigmodel.cn/api/paas/v4`) | The coding plan has its own address, `https://api.z.ai/api/coding/paas/v4`: change the base URL after choosing `glm`. `glm-cn` is priced in CNY, which needs an exchange rate (section 12, Currencies), and its prices rise with the input length: enter them as tiers. Earlier reasoning reaches the model only with preserved thinking: set the quirk `request_fields` to `{"thinking": {"clear_thinking": false}}`. A quota error (codes 1113, 1308–1311 and 1313–1321) becomes 402 `upstream_quota_exhausted`; a rate limit (1302) or an overload (1305) stays 429 and is retried. LiteLLM lists GLM under the key prefix `zai`. Capability presets `glm-5`, `glm-5-vision`, `glm-4`. |
| Kimi | `kimi` (`https://api.moonshot.ai/v1`), `kimi-cn` (`https://api.moonshot.cn/v1`) | A key works only in the region that issued it. The gateway leaves sampling parameters out (Kimi fixes them) and always sends earlier reasoning back. LiteLLM lists Kimi under the key prefix `moonshot`. Capability presets `kimi-k3`, `kimi-k2`. Checked against Kimi's documentation and the mock only. |
| xAI | `xai` (`https://api.x.ai/v1`) | Grok's reasoning tokens are reported apart from `completion_tokens`; the gateway bills them as output. The reasoning models refuse `stop` and the penalties, which their capability presets leave out. xAI marks Chat Completions as deprecated: the managed interfaces use it while it lasts, and its Responses API is reached through proxy mode. LiteLLM lists xAI under the key prefix `xai`. Capability presets `xai-reasoning`, `xai-mini`, `xai-chat`. Checked against xAI's documentation and the mock only. |
| Ollama | `ollama` (local), `ollama-cloud` (`https://ollama.com/v1`) | A local server needs no key. Inside the stack, `localhost` is the container: use the host's address (`http://host.docker.internal:11434/v1` on Docker Desktop) and add it to `PGW_EGRESS_ALLOW_PRIVATE`. Keep the server reachable from the gateways only. Discovery reads each model's capabilities from `/api/show` and proposes local models as free. Ollama Cloud needs a key from ollama.com, sent as Bearer; its models are not proposed as free. Capability presets `ollama`, `ollama-thinking`. Checked against Ollama's documentation and the mock only. |

`make smoke-live` checks a live installation's providers with the keys in `.env`
(`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `ZAI_API_KEY`,
and for Vertex AI `VERTEX_PROJECT`, `VERTEX_LOCATION` and `VERTEX_CREDENTIALS_FILE`, the path
of a service account's key file kept outside the repository); a provider whose key is missing
is skipped and named.

### Google's SDKs

Google's Gen AI SDKs call the gateway's Gemini interface with their base URL set to the
gateway's root, and reach every model the key may use, not only Gemini. Name the model as the
gateway does (`gemini/gemini-3.8-flash`, `anthropic/claude-sonnet-5-5`, `@template`).

- **Gemini API mode:** the gateway key as the API key.
  Python: `genai.Client(api_key=KEY, http_options=types.HttpOptions(base_url="https://<API host>"))`.
- **Vertex AI mode, express:** `genai.Client(vertexai=True, api_key=KEY, http_options=...)`.
- **Vertex AI mode with a project:** the SDK sends a credential's token as a Bearer token, so
  give it the gateway key as one:
  `genai.Client(vertexai=True, project="any", location="any", credentials=google.oauth2.credentials.Credentials(token=KEY), http_options=...)`.
  The project and location the SDK names are not used: the provider's are.

Streams, token counting and embeddings work in both modes. A model on Gemini's API receives
the request as written, apart from what the gateway governs; any other model, Vertex AI's
included, through translation. Files and context caches belong to a provider account: use
proxy mode for them.

## 5. Health checks

Every service has a health check. `dc ps` shows the result. Compose starts a service only
when the services it needs are healthy.

| Service | Check | Healthy means | Unhealthy usually means |
|---|---|---|---|
| `preflight` | exit status | All required secrets are set and well-formed. | `dc logs preflight` names the missing or malformed value. |
| `postgres` | `pg_isready` over TCP | The server accepts connections. | Still initialising, wrong volume ownership, disk full. |
| `clickhouse` | `GET /ping` | The server answers. | Still initialising, out of memory, disk full. |
| `core` | `GET :8080/readyz` | A configuration snapshot is loaded and the spool accepts events. | PostgreSQL unreachable at start, a missing secret, the spool's disk takes no writes (read-only, permissions), or the spool is full while the installation-wide `on_spool_full` is `fail_closed` (section 10, "The spool grows"). |
| `analytics` | `GET :8090/readyz` | The ClickHouse schema is in place. | ClickHouse unreachable or wrong password. The service keeps retrying. |
| `ui` | `GET :3000/` | The console answers. | The process crashed; see `dc logs ui`. |
| `id-db` | exit status | pacod-id's role and database exist and the role has `PID_DB_PASSWORD`. | PostgreSQL refused `POSTGRES_PASSWORD`; see `dc logs id-db`. |
| `id` | `GET :9170/readyz` | pacod-id has migrated its database and loaded its signing key. | A missing or wrong `PID_*` setting, or a mail setting it refuses (section 10, "Signing in fails with pacod-id"); see `dc logs id`. The core waits for it. |
| `caddy` | its admin endpoint | Caddy runs with a loaded configuration. | An error in the Caddyfile or in a variable it uses. |
| `prometheus` | `GET /-/ready` | Prometheus has loaded its data. | A broken rule file, or the volume is full. |
| `grafana` | `GET /api/health` | Grafana and its database are up. | First start still running (up to five minutes are allowed). |

The two endpoints of the core and the analytics service differ on purpose:

- `/healthz` answers as soon as the process runs. It says nothing about dependencies. A
  failure means the process hangs or is gone.
- `/readyz` answers 200 only when the service can do its work. The core's data plane
  answers 503 with `"status": "event spool full"` when it refuses requests, and with
  `"status": "event spool cannot be written"` when the last probe could not write, sync
  and remove a small file in the spool directories (checked every five seconds, whatever
  `on_spool_full` says: the fault is this instance's disk). The management
  API (port 8081) answers 503 with `"status": "database unreachable"` when it cannot reach
  PostgreSQL. With the role `admin` the core serves both endpoints on 8081 instead of 8080.

A load balancer in front of several instances should use `/readyz`. Caddy does.

The `otel-collector` has no health check in compose because its image contains no tool to
run one. Its own endpoint is `http://otel-collector:13133/` inside the network.

## 6. Upgrade

1. Read the release notes of the new version.
2. Make a backup (section 7).
3. Get the new images: build them with the new tag, or let Docker pull them. Released
   images are on `ghcr.io/<owner>/pacod-gw-<name>`, signed with cosign and with a signed
   bill of materials; check a signature before you run an image:

   ```sh
   cosign verify ghcr.io/<owner>/pacod-gw-core:<version> \
     --certificate-identity-regexp 'https://github.com/<owner>/<repository>/.github/workflows/release.yml@refs/tags/v.*' \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com
   ```

   Set `PGW_REGISTRY=ghcr.io/<owner>/` in `deploy/.env` to pull them.
4. Set `PGW_VERSION` in `deploy/.env` to the new tag.
5. Apply:

   ```sh
   git pull            # the compose file, Caddyfile, dashboards and rules belong to the release
   dc pull --ignore-pull-failures   # images built on this host cannot be pulled; that is fine
   dc up -d --wait
   ```

Compose replaces only the containers whose image or settings changed. Files mounted into a
container, such as the Caddyfile and the Prometheus rules, are read again only on a restart:
after a release that changes `deploy/caddy` or `deploy/prometheus`, run
`dc restart caddy prometheus`. The core migrates
the PostgreSQL schema when it starts, and the analytics service migrates ClickHouse. No
separate migration step is needed. To apply the PostgreSQL migrations without starting the
gateway, run `dc run --rm core migrate`.

**Order: analytics before the core.** When a release adds fields to the usage event, the
analytics service must know them before the core sends them. An older analytics service
accepts the newer events but drops the fields it does not know, and they cannot be recovered
later. With Compose, `dc up -d --wait analytics` first, then `dc up -d --wait`; with Helm,
roll out the analytics Deployment first. The analytics service answers `503` until its own
ClickHouse migrations have run, and the core keeps the events in its spool until then.
The ClickHouse migration `0003_attribution` (the caller's `agent_id`, `user_id`, `task` and
`complexity`) is such a change.

**Upgrading to the release with pacod-id.** This release adds the services `id` and `id-db`
(section 16), and they run whatever `PGW_AUTH` says. Before step 5:

- Add pacod-id's secrets to `deploy/.env`, generated as in section 4, step 3:
  `PID_DB_PASSWORD`, `PID_KEK`, `PID_TOKEN_PEPPER`, `PGW_ID_CLIENT_SECRET`,
  `PGW_UI_CLIENT_SECRET`, `TESTER_ID_CLIENT_SECRET` and `PGW_UI_SESSION_SECRET`. They are
  required even with `PGW_AUTH=local`: without them preflight stops the start and
  `dc logs preflight` names each one. Store `PID_KEK` and `PID_TOKEN_PEPPER` apart from the
  backups (section 3).
- Set `PID_KEK_ID` before `id` first starts (for example `2026-10`); preflight does not check
  it, and unset it is `default`. pacod-id seals its signing keys under the id in effect, and a
  later change without the old key in `PID_KEK_OLD` stops it, and the core with it.
- Set `PGW_ID_HOST` to a DNS name that points at the host; Caddy obtains its certificate.
  Preflight does not catch a missing one: compose then uses `id.localhost`, which is of no use
  for signing in from other machines.
- Get the pacod-id image of the same `PGW_VERSION`; `make images` builds it.
- With `PGW_SMTP_HOST` set, also set `PGW_EMAIL_FROM_ADDRESS`, and set `PGW_SMTP_PASSWORD`
  whenever `PGW_SMTP_USERNAME` is set. The core takes the user name as the sender when the
  address is empty; pacod-id receives the same settings and refuses to start without a sender
  address, or with a user name but no password. It also takes `PGW_SMTP_TLS` only in lower
  case (`starttls`, `tls`, `none`), where the core accepts any case.
- Leave `PGW_AUTH` unset (or `local`): the console keeps its own login and its people. Moving
  them to pacod-id is a step of its own, at a time you choose (section 16).

`id-db` creates pacod-id's role and database in the existing PostgreSQL at the first start;
with a PostgreSQL the compose file does not run, see section 16. The core now starts only
once `id` is healthy, in either mode, so a pacod-id that cannot start also keeps the core
from starting (section 10, "Signing in fails with pacod-id"). The release changes
`deploy/caddy` and `deploy/prometheus`: run `dc restart caddy prometheus` after step 5.
Afterwards `dc ps` shows `id` healthy and `id-db` exited (0).

During the replacement of the core the data plane is down for the time the new container
needs to become ready. Running streams get up to 50 seconds to finish. For an upgrade
without that gap, run several gateway instances and replace them one after the other
(section 9, "Upgrading several instances").

**Rolling back.** Set `PGW_VERSION` back and run `dc up -d --wait`. This works as long as
the newer version made no schema change the older one cannot read; the release notes say
so. Otherwise restore the backup from step 2.

**Upgrading the third-party images** (PostgreSQL, ClickHouse, Caddy, Prometheus, Grafana,
the collector): their tags are pinned in `deploy/docker-compose.yml` and change with a
pacod-gw release. A new major version of PostgreSQL needs `pg_upgrade` or a dump and
restore; do not just change the tag.

## 7. Backup and restore

### What is backed up

| Data | In the backup | Why |
|---|---|---|
| PostgreSQL | Yes, `postgres.dump` | Configuration, encrypted credentials, audit log, counters. Losing it means setting everything up again. |
| pacod-id's database `pacod_id` | Yes, `pacod-id.dump`, when it exists | With `PGW_AUTH=oidc` the people, their roles and admin tokens live there. Its signing keys are sealed under `PID_KEK`, which is not in the backup either. |
| ClickHouse | Yes, `clickhouse.zip` | Usage and cost history, audit-mode records. |
| `deploy/.env`, including `PGW_KEK` | **No** | The key must be kept apart from the encrypted data (section 3). |
| Spool | No | Transient. It empties itself as events are shipped. |
| Caddy certificates | No | Caddy obtains them again. |
| Prometheus, Grafana | No | Metrics history only; the dashboards come from the repository. |

### Making a backup

```sh
deploy/scripts/backup.sh
```

The script names the fingerprint of the key-encryption key in use and asks whether a copy
of the key is stored separately. It then writes a directory `deploy/backups/<UTC time>/`
with the two dumps, a manifest and checksums. Nothing is stopped; both dumps are consistent
snapshots.

Options:

| Option | Effect |
|---|---|
| `--kek-copy FILE` | Verifies your separately stored copy: `FILE` holds the key and nothing else. The backup runs only if it matches the key in use. Use this where the copy can be mounted or fetched. |
| `--kek-stored` | States that the copy exists, without verifying. For scheduled runs. |
| `--output DIR` | Writes the backup below `DIR`. |
| `--keep N` | Deletes all but the newest `N` backups afterwards. |
| `--skip-clickhouse` | PostgreSQL only. |

A nightly run with cron, keeping two weeks:

```cron
15 2 * * * cd /opt/pacod-gw && deploy/scripts/backup.sh --kek-stored --keep 14 >> /var/log/pacod-gw-backup.log 2>&1
```

**Copy the backup directory off the host.** A backup that lives only on the machine it
protects is not a backup. Do not copy `deploy/.env` to the same place.

Test a restore on a second machine from time to time. That is the only proof that the
backup and the stored key work together.

### Restoring

On a fresh host:

1. Do steps 1 to 3 of the first deployment, but put the **original** `PGW_KEK`,
   `PGW_KEK_ID` and `PGW_KEY_PEPPER` into `deploy/.env`, from where you stored them. The
   other secrets may be new ones, except pacod-id's `PID_KEK`, `PID_KEK_ID` and
   `PID_TOKEN_PEPPER` when the backup holds `pacod-id.dump`: `restore.sh` restores pacod-id's
   database too, and with a new `PID_KEK` pacod-id cannot read its restored signing keys and
   does not start. The script does not check `PID_KEK`.
2. Copy the backup directory to the host.
3. Run:

   ```sh
   deploy/scripts/restore.sh /path/to/backup/20261004T021500Z
   ```

The script checks the checksums, compares the key in `deploy/.env` with the fingerprint in
the backup's manifest, starts only PostgreSQL and ClickHouse, restores both, and then starts
the whole stack. The core and the analytics service bring the schemas up to their own
version, so a backup from an older release can be restored into a newer one.

It refuses to continue, and changes nothing, when:

- the key-encryption key in `deploy/.env` is not the one the backup was made under. Fix
  `PGW_KEK`. (`--skip-kek-check` restores anyway; the provider credentials then stay
  unreadable until the right key is set.)
- the databases already hold tables. This protects a running installation. To replace its
  data with the backup, add `--force`; everything newer than the backup is lost.

After a restore, log in to the console and run the probe of one provider. A probe that
fails with a decryption error means the key does not belong to the restored data.

If `PGW_KEY_PEPPER` is not the original one, the restore succeeds but no gateway key or
admin token is accepted. Set the original value and restart the core.

**With several instances**, set `COMPOSE_FILE` to the files the stack runs with, for example
`COMPOSE_FILE=deploy/docker-compose.yml:deploy/docker-compose.scale.yml`; the script then
stops the local gateways too. Stop gateways on other machines yourself before the restore and
start them again afterwards. A running instance notices a restored configuration (an older
version than its own) within a second and loads it.

## 8. Rotating keys

### Provider keys

No restart is needed. In the console: Platform, Providers, the provider, Credentials, add a
credential and make it current. The next request uses the new key; calls already running
finish on the old one. The previous credential is kept as `retiring` for a grace period
(`grace_hours`, 24 by default), during which it can be made current again; the next
housekeeping round after that disables it. Revoke the old key at the provider after the
grace period.

An organisation's or a workspace's own keys are kept by its admins on its settings page
(Provider keys): adding one, making one current, disabling and deleting work the same way,
for that organisation or workspace only. A workspace's calls use its own key first, then its
organisation's, then the installation's shared one.

The same through the management API (see section 4 for how to reach it):

```sh
curl -X POST http://core:8081/api/v1/providers/<provider id>/credentials \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "content-type: application/json" \
  -d '{"label":"2026-10 rotation","secret":"<new provider key>","make_current":true}'
```

The answer's headers show the new configuration version (`x-gw-config-version`) and on how
many instances it is applied (`x-gw-applied`, section 12).

### Gateway keys

In the console: the workspace, API keys, Rotate. The new key is shown once. Revoking a key
takes effect within two seconds on every instance.

### The key-encryption key

Rotate it when someone who knew it leaves, when it may have leaked, or on a schedule. Old
records stay readable during the rotation because the old key is kept as a decryption key.

1. Generate the new key and **store it separately first** (section 3):

   ```sh
   openssl rand -base64 32
   ```

2. Edit `deploy/.env`:

   ```sh
   PGW_KEK_OLD=<old PGW_KEK_ID>=<old PGW_KEK>      # append with a comma if it is not empty
   PGW_KEK=<new key>
   PGW_KEK_ID=<new name, for example 2027-01>
   ```

3. Restart the core: `dc up -d --wait core`, and every other instance with the same three
   values: the local gateways of the scale layout and gateways on other machines (section
   9). From now on every credential that is added or rotated is encrypted under the new
   key. Existing ones are still read with the old key.

4. Once **every** instance runs with the new key, wrap the existing credentials under it.
   An instance still on the old key could not read them afterwards, so both ways below
   refuse while a running instance has not confirmed the new key, and name it. Each credential's data key is
   decrypted with the old key and encrypted with the new one; the secrets themselves are not
   touched. Either run the command once:

   ```sh
   dc run --rm core rewrap-secrets
   ```

   or, as a platform admin, call `POST /api/v1/system/rewrap-secrets`. Both report how many
   credentials were wrapped and list any that could not be read (their key is missing from
   `PGW_KEK_OLD`). The command exits non-zero when one could not be read. Running it again
   is safe: credentials already under the new key are left alone. Every change is in the
   audit log as `rewrap`. The list of credentials shows for each one the `kek_id` it is
   encrypted under.

5. When no credential shows the old `kek_id` any more, remove the old key from
   `PGW_KEK_OLD` and restart the core. Keep the old key in your secret store for as long as
   you keep backups made under it: restoring such a backup needs it.

A backup records which key it was made under. `restore.sh` accepts the key as `PGW_KEK` or
as an entry of `PGW_KEK_OLD`.

### Other secrets

| Secret | How |
|---|---|
| `PGW_INTERNAL_TOKEN` | Change it in `deploy/.env`, then `dc up -d --wait core analytics`. Events wait in the spool while the two restart. |
| `POSTGRES_PASSWORD` | `dc exec postgres psql -U pacod_gw -d pacod_gw -c "ALTER ROLE pacod_gw PASSWORD '<new>'"`, then change `deploy/.env`, then `dc up -d --wait core`. The variable alone does not change the password of an existing volume. |
| `CLICKHOUSE_PASSWORD` | Change it in `deploy/.env`, then `dc up -d --wait clickhouse analytics`. The ClickHouse image writes the user from the variable whenever its container is created. |
| `PGW_KEY_PEPPER` | Do not rotate it. Changing it invalidates every gateway key and admin token at once. If it has leaked, change it and issue all keys again. pacod-id receives it as `PID_LEGACY_PEPPERS` for the imported `pgwa_` admin tokens; compose passes the new value to both. |
| `PID_TOKEN_PEPPER` | Do not rotate it either. Changing it invalidates every `pidt_…` admin token made with `PGW_AUTH=oidc`; imported `pgwa_…` tokens are checked with `PGW_KEY_PEPPER` and survive. |
| pacod-id's secrets | `vendor/pacod-id/docs/runbook.md`: section 11 for `PID_KEK`, section 12 for the client secrets. Change a client secret in `deploy/.env` and run `dc up -d --wait`, so that pacod-id and its client restart with the same value. `PGW_UI_SESSION_SECRET` can change at any time; everyone is signed out of the console. |
| `GRAFANA_ADMIN_PASSWORD` | Change the password in Grafana. The variable is read only when Grafana creates its database. |
| TLS certificates | Caddy renews them. Nothing to do. |

## 9. Scaling

The design target for one core instance with 2 CPUs and 1 GB is 10 000 concurrent streams
in proxy mode and 5 000 in managed mode. A load test has not confirmed these numbers yet.
Look at your own numbers before adding instances: the dashboard "Gateway internals" shows
streams in flight, rejections with reason `gateway_overloaded`, and the gateway delay. A load
test from the console measures them (below). Several instances also let you upgrade without a
gap in the data plane.

### What several instances share

The instances share one PostgreSQL and nothing else. A core has one of three roles
(`PGW_ROLE`): `gateway` serves the data plane, `admin` the management API and the console,
`all` both.

- **Configuration.** A change made in the console reaches every instance within about two
  seconds. The console shows "applied by all N running instances", and the System page
  shows the version each instance has applied.
- **Limits.** Each instance counts in memory and combines its counts with the others
  through PostgreSQL once a second; it keeps a copy of every counter of a running window,
  also of subjects it has not served yet, read when it starts. A limit can be overshot by the
  calls in flight plus about one second of traffic. A reset in the console reaches every
  instance within two seconds.
- **A key's concurrency cap** (`max_concurrency`) is shared out: with N gateways each allows
  `ceil(max / N)` calls of that key at once. With uneven load a key can be refused early on
  one gateway; a cap below N allows N.
- **The console sees every instance.** Each instance writes its live figures to PostgreSQL
  with its heartbeat, every five seconds. Monitoring adds them up and lists each instance
  (click one to see it alone); the System page lists the instances, and those that stopped
  reporting in the last day. The management API never calls a gateway, so a gateway on
  another machine needs only outgoing connections to the central services.
- **Each instance has a spool of its own** and must have its own volume. A second process
  that tries to use the same spool directory refuses to start.
- **Each instance has a name**, `PGW_INSTANCE_ID`. Without it, an instance takes a name the
  first time it starts and keeps it in its spool directory. A second running process with
  the same name refuses to start; one that already runs is reported (see
  [An instance id is in use twice](#an-instance-id-is-in-use-twice)).
- **Secrets.** `PGW_KEK`, `PGW_KEK_ID`, `PGW_KEK_OLD`, `PGW_KEY_PEPPER` and
  `PGW_INTERNAL_TOKEN` must be the same on every instance. The console raises a critical alert
  for an instance whose pepper or key-encryption key differs.

What every instance keeps for itself, by design:

| State | Effect with N instances |
|---|---|
| Circuit breakers, time to first byte, calls in flight per model | Each instance learns on its own. Monitoring, Routing shows each instance's view. |
| Round-robin turns of a routing pool | Each instance rotates on its own; the spread is even over time. |
| Bad-key throttle (60 failed keys a minute per client) | Per instance: a client may try N × 60 bad keys a minute in all. |
| Login and email throttles of the console | Per admin instance. |

### Layouts

| Layout | Files | |
|---|---|---|
| One core | `docker-compose.yml` | The default: one core with the role `all`. |
| Several gateways on one host | `+ docker-compose.scale.yml` | `core` becomes the management instance, `gateway-1` and `gateway-2` serve the data plane. |
| Gateways on other machines | `+ docker-compose.hub.yml` on the central host, `deploy/node/` on each other machine | One central console and management instance; any number of gateways. |
| Kubernetes | `deploy/helm/pacod-gw` | A gateway StatefulSet and an admin Deployment. |
| Minimal gateway | `deploy/lite/docker-compose.yml` | A separate installation without database or console (section 17). Several instances share nothing: each counts its own limits. |

In every layout Caddy on the central host balances the data plane over the gateways named
in `PGW_DATA_UPSTREAMS`: each request goes to the gateway with the fewest calls in flight,
only to gateways that answer `/readyz`, and a request whose gateway cannot be reached is
sent to another one.

### Several gateways on one host

```sh
dcs() { docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.scale.yml "$@"; }
dcs up -d --wait          # or: make scale-up
```

`core` keeps its name, so the console, the analytics service and the backup scripts need no
change; it serves only the management API now. The tester calls the data plane through
Caddy, as applications do.

To add a gateway, copy the `gateway-2` block in `deploy/docker-compose.scale.yml`, give it
the next number and a volume of its own, add it to
`deploy/prometheus/targets-scale/gateways.yml`, and set
`PGW_DATA_UPSTREAMS="gateway-1:8080 gateway-2:8080 gateway-3:8080"` in `deploy/.env`.
Then `dcs up -d --wait`.

Check: the console's System page lists `core` (role admin) and the gateways, all on the
newest configuration version. `dcs config --volumes` lists one spool volume per gateway.

### Gateways on other machines

```
                      clients
                         │ HTTPS
┌────────────────────────▼─────────── central host ──────────────────────────────────┐
│  Caddy ──── console (ui) ──── core (admin)       PostgreSQL   analytics   OTLP     │
│    │                                             Prometheus                        │
└────┼────────────────────────────────────────────────▲─────────────▲────────────────┘
     │ 8080                                           │ 5432, 8090, 4318
     │            private network (VLAN, VPC, VPN)    │ (outgoing from the gateways)
┌────▼─────────────┐   ┌──────────────────┐           │
│ gateway-eu-1     │   │ gateway-eu-2     │ ──────────┘
│ deploy/node/     │   │ deploy/node/     │   + 9100 from the central Prometheus
└──────────────────┘   └──────────────────┘
```

The machines must share a private network: a VLAN, a cloud VPC, or a VPN such as
WireGuard. The central host offers PostgreSQL, the analytics service and the trace
collector on its private address only; the gateways offer their data plane and metrics on
theirs. Nothing of this belongs on the internet.

| From | To | Port | What |
|---|---|---|---|
| gateways | central host | `PGW_CLUSTER_POSTGRES_PORT` (5432) | PostgreSQL |
| gateways | central host | `PGW_CLUSTER_ANALYTICS_PORT` (8090) | usage events (needs `PGW_INTERNAL_TOKEN`) |
| gateways | central host | `PGW_CLUSTER_OTLP_PORT` (4318) | traces |
| central host | gateways | `PGW_NODE_DATA_PORT` (8080) | Caddy, the data plane |
| central host | gateways | `PGW_NODE_METRICS_PORT` (9100) | Prometheus |
| gateways | providers | 443 | the LLM providers |

**On the central host:**

1. In `deploy/.env`, set `PGW_CLUSTER_BIND` to the host's private address, and the ports
   if 5432, 8090 or 4318 are taken there.
2. Start the stack with the hub file, alone or with the scale file:

   ```sh
   dch() { docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.scale.yml \
             -f deploy/docker-compose.hub.yml "$@"; }
   dch up -d --wait
   ```

**On each other machine** (Docker and a checkout of the repository at the same release):

3. Get the core image of the same `PGW_VERSION`: build it (`docker build -f
   deploy/docker/rust.Dockerfile --target core -t pacod-gw-core:<version> .`) or pull it from
   your registry (`PGW_REGISTRY`).
4. `cp deploy/node/.env.example deploy/node/.env` and fill it in: a unique
   `PGW_INSTANCE_ID`; the machine's private address as `PGW_NODE_BIND`; the central host's
   private address in `DATABASE_URL`, `PGW_ANALYTICS_URL`, `PGW_OTLP_ENDPOINT` and
   `PGW_TRUSTED_PROXIES`; and the shared secrets, copied from the central `deploy/.env`.
   `chmod 600 deploy/node/.env`.
5. Start it:

   ```sh
   docker compose -f deploy/node/docker-compose.yml up -d --wait
   ```

   The preflight step names anything missing. Within a few seconds the console's System page
   lists the gateway with the newest configuration version. An alert about different
   secrets means step 4 copied a wrong value.

**Back on the central host:**

6. Add the gateway to `PGW_DATA_UPSTREAMS` in `deploy/.env`, listing every gateway, local
   ones included: `PGW_DATA_UPSTREAMS="gateway-1:8080 gateway-2:8080 10.0.0.11:8080"`. Then
   `dch up -d caddy`.
7. Copy `deploy/prometheus/targets/nodes.yml.example` to `nodes.yml` beside it and list the
   gateway's metrics address with its instance id. Prometheus reads the file within 30
   seconds.
8. Check: the console's Monitoring page lists the gateway, and its figures move when calls
   arrive.

Every provider must be reachable from every machine. A provider on a private address must be
reachable from each gateway and named in each gateway's `PGW_EGRESS_ALLOW_PRIVATE`.

**Removing a gateway:** take it out of `PGW_DATA_UPSTREAMS` and run `dch up -d caddy`
first. Then stop it with `docker compose -f deploy/node/docker-compose.yml down`; it signs
off and leaves the console's instance list. Keep its volume until its events have been
delivered: Monitoring shows "Waiting to be shipped" per instance. A gateway that is killed
instead appears under "Stopped reporting" for a day.

**When the central host is down,** the console and the management API are down, and so is
the data plane: Caddy, the entry point for clients, runs there. The gateways themselves keep
serving on the last configuration they loaded, count limits locally, and keep their events
in the spool. A gateway that restarts while PostgreSQL is unreachable cannot start. To keep
the data plane up without the central host, put a second load balancer (a cloud load
balancer, or DNS over the gateways) in front of the gateways' data-plane ports; that is
outside this setup.

**TLS between the machines** is optional on a private network. For PostgreSQL: mount a
certificate and key into the `postgres` service with a further override file (the key owned
by the `postgres` user, mode 0600), add `-c ssl=on -c ssl_cert_file=... -c
ssl_key_file=...` to its command, and append `?sslmode=require` (or `verify-full` with
`sslrootcert=`) to the gateways' `DATABASE_URL`. This has not been exercised.

### Connections to PostgreSQL

PostgreSQL accepts 100 connections (`max_connections` in `docker-compose.yml`). A core with
the role `admin` or `all` keeps up to 10, a gateway up to 4 (`PGW_DB_MAX_CONNECTIONS`
changes either). A backup or a `psql` session needs a few more. With one admin instance
that leaves room for about 20 gateways; for more, raise `max_connections` and the memory of
the `postgres` service.

### Upgrading several instances

Upgrade one instance after the other; the data plane stays up.

1. The analytics service first (section 6).
2. The management instance: `dcs up -d --wait core`. It applies the PostgreSQL migrations.
3. Each gateway, one at a time: `dcs up -d --wait gateway-1`, then `gateway-2`, then on each
   other machine `docker compose -f deploy/node/docker-compose.yml up -d --wait`. A gateway
   that is told to stop answers `/readyz` with 503 for six seconds
   (`PGW_DRAIN_DELAY_SECONDS`) while it still serves, so Caddy stops sending it requests,
   and then lets running calls finish.

Migrations are written so that the previous release keeps working on the newer schema, and
an instance accepts migrations it does not know. An instance also keeps configuration
objects that a newer release wrote with fields it does not know: it ignores those fields.
The management API is the one that refuses unknown fields on templates and routing schemes.
One exception: binaries built before the
cluster migration (`0006_cluster`) do not accept unknown migrations. Once the management
instance has applied it, such a gateway keeps running but cannot restart until it is
upgraded too.

### Load testing before scaling

The console's load tests ([testing.md](testing.md), section 9) put a set load on the gateway
and show, second by second, what it serves, how fast, and what fails. To measure capacity:

1. Start the stack with `docker-compose.load.yml` ("make up LOAD_TEST=1", or add it after the
   scale or hub files), with `TESTER_LOAD_TOKEN` in deploy/.env and `load-mock` in
   `PGW_EGRESS_ALLOW_PRIVATE`. In the scale layout set `LOAD_AGENT_TARGET_URL=http://caddy:8088`.
2. Run one or more agents on other machines (below), aimed at the public API host, so that
   the load comes through Caddy and TLS as applications' calls do, and takes no CPU from the
   gateway.
3. Load the mock with a ramp, in calls a second, until calls are not started or the gateway
   answers 503 `gateway_overloaded`. The agents' CPU must stay below the warning line; add
   agents otherwise.
4. Compare the gateway's own delay and its calls in flight with the dashboard
   "Gateway internals".

A load test against the mock costs nothing but load. The usage events of every call reach
analytics: watch for dropped events, and expect ClickHouse to grow by the calls of the test.

### A load agent on another machine

`deploy/load-agent/` runs one agent. On that machine:

```sh
cp deploy/load-agent/.env.example deploy/load-agent/.env && chmod 600 deploy/load-agent/.env
# LOAD_AGENT_NAME, LOAD_AGENT_BIND (the private address), LOAD_AGENT_TOKEN (the central
# TESTER_LOAD_TOKEN), LOAD_AGENT_TARGET_URL (the API host as this machine reaches it)
make load-agent-up
```

On the central host, name it in `TESTER_LOAD_AGENTS` (`local=http://load-agent:8096,eu-1=http://<private address>:8096`)
and restart the tester. The console then lists it under Load, with its CPUs, its caps and
the address it reaches the gateway at.

- The tester reaches the agent, never the other way round: allow the central host to reach
  port 8096 there, and nothing else.
- The link is plain HTTP and carries the token and the test's key: use the private network or
  VPN of the gateways on other machines.
- A gateway behind a private CA: put its certificate in `deploy/load-agent/ca/` and name the
  file in `LOAD_AGENT_CA_FILE_IN_CONTAINER`.
- The agent needs a file limit of one socket per call in flight; the compose file sets
  65 536.

### On Kubernetes

Use the Helm chart (`deploy/helm/pacod-gw`, see its README). The gateway is a StatefulSet;
set `gateway.replicas`. Each instance gets its own spool volume, the pod name is its
instance id, the admin role runs as its own Deployment, and the Ingress balances across the
instances that answer `/readyz`. `core.trustedProxies` (default `private_ranges`) lets the
gateways see the client's address behind the ingress controller. The ServiceMonitor labels
every series with the pod name. The admin pods keep their spool in an `emptyDir`
(`admin.spoolSizeLimit`, above the spool caps): usage events of playground calls and of
settled message batches that have not been delivered are lost when an admin pod is
replaced. They are delivered within seconds while the analytics service is up.

### The stores

PostgreSQL carries configuration and counters, not request payloads; its load is small.
ClickHouse grows with the number of requests. Watch the disk of the `clickhouse-data`
volume and set a retention for usage events and audit-mode records in the console.

Usage events are kept until a retention says otherwise: `usage_retention_days` in the
global settings, an organisation's settings (which its workspaces inherit unless they set
their own, and which it can lock) or a workspace's settings; 0 keeps them. Once a day the
analytics service deletes, for each workspace, the events older than its retention,
counted in whole days from midnight UTC; the installation's retention also covers events of
workspaces that no longer exist. Lowering a retention therefore also removes stored events.
Each run is announced on the live event feed as job `retention`, and kept with what it
deleted in the `job_runs` table of PostgreSQL.

## 10. When something fails

Start with these three:

```sh
dc ps                          # which service is not healthy?
dc logs --tail 100 <service>   # what does it say?
dc logs preflight              # are the secrets complete?
```

The core, the analytics service and Caddy write one JSON object per line
(`PGW_LOG_FORMAT=json`), without request bodies or keys. Docker keeps 5 files of 10 MB per
service.

Where to look:

| Question | Place |
|---|---|
| Is the gateway up and what version of the configuration runs? | `curl https://<API host>/readyz` |
| What is failing right now? | Console: Monitoring. Prometheus alerts: `http://prometheus:9090/alerts` inside the network, or Grafana. The gateway-delay chart draws the targets of the global setting `delay_targets` (proxy and managed mode) as dashed lines. |
| What happened to one request? | Console: Requests. Search by the `x-gw-request-id` response header. |
| Who changed the configuration? | Console: System, audit log. |
| Trends | Console: Metrics (platform roles; needs `PROMETHEUS_URL` for the console, set in the compose file, `ui.prometheusUrl` in the Helm chart). Grafana: Overview; Providers and models; Workspaces; Gateway internals, the same panels. |
| Traces | The collector logs a summary (`dc logs otel-collector`). For full traces, configure a backend in `deploy/otel-collector/config.yaml`. Each request has a span, and each provider attempt a child span with the provider's request id. |

### Alert delivery

Prometheus evaluates the alert rules and hands what fires to the stack's Alertmanager, which
sends it by email and to a webhook when `deploy/.env` says where:

- `PGW_ALERT_EMAIL_TO`: one or more addresses, separated by commas. The mail goes out with
  the SMTP account of the account emails (`PGW_SMTP_*`, section 13); without
  `PGW_SMTP_HOST` nothing is sent by email.
- `PGW_ALERT_WEBHOOK_URL`: an HTTP endpoint that receives Alertmanager's JSON.

Both empty: the alerts are shown in Prometheus, Grafana and the console only. The
configuration is written from these values at every start of the `alertmanager` service
(`deploy/alertmanager/render.sh`); change them and run `dc up -d alertmanager`. The
development stack publishes Alertmanager on port 19093. With the Helm chart, set
`metrics.alertmanagerConfig` (an AlertmanagerConfig for the Prometheus Operator's
Alertmanager).

The console's Monitoring page lists the same conditions as open alerts, computed by the
core from the last minutes of every instance: over 15 minutes, error rate above 10 % with at
least 20 calls and more than five timeouts; over 10 minutes, more than one rejection a
second for one reason and the gateway delay's 99th percentile above its target; a nearly full audit spool, besides limits, prices,
providers, spools, configuration and instances.

### The stack does not start

- `service "preflight" didn't complete successfully`: `dc logs preflight` names each secret
  that is missing, too short or malformed. Fix `deploy/.env` and start again.
- `core` restarts in a loop: `dc logs core`. The message names the variable
  (`PGW_KEK is not set`, `DATABASE_URL is not set`, `PGW_INTERNAL_TOKEN must be set`).
  `password authentication failed` means `POSTGRES_PASSWORD` was changed after the volume
  was created (section 8).
- `caddy` is unhealthy: `dc logs caddy`. With Let's Encrypt, the two DNS names must point
  to this host and ports 80 and 443 must be reachable from the internet.
- A port is in use: set `PGW_HTTP_PORT` and `PGW_HTTPS_PORT`. Let's Encrypt needs the
  standard ports.
- Compose reports the `id` container as unhealthy, or never starts the core: the core waits
  for pacod-id in either mode. See "Signing in fails with pacod-id" below.

### Logging in to the console fails

This is the console's own login (`PGW_AUTH=local`). With `PGW_AUTH=oidc`, see the next
subsection.

- "Cross-site POST form submissions are forbidden": the console builds its own address
  from the `Host` header and `X-Forwarded-Proto`. Something between the browser and the
  console changes `Host` or does not set the protocol. Open the console only through Caddy,
  under `PGW_CONSOLE_HOST`. If another proxy or load balancer sits in front of Caddy, list
  it under `trusted_proxies` in the Caddyfile and raise `XFF_DEPTH` of the `ui` service by
  one for every such hop.
- The login is accepted but you are logged out at once: the session cookie is marked
  secure and the browser reached the console over plain HTTP.
- No administrator exists: `PGW_ADMIN_EMAIL` and `PGW_ADMIN_PASSWORD` are read only when
  the database has no user. The core logs a warning at start when neither exists.
- "Confirm your email address first": the user has not opened the verification link yet.
  The login page offers a new link; a platform admin can send one or mark the address as
  confirmed (Platform, Access, Users).
- A reset or verification email does not arrive: see section 13.

### Signing in fails with pacod-id

With `PGW_AUTH=oidc` (section 16). pacod-id's own failures and their messages are in
`vendor/pacod-id/docs/runbook.md`, section 18.

- **`id` does not become healthy** (`dc logs id`). It names the variable: a missing `PID_*`
  secret or client secret, `PID_EMAIL_FROM_ADDRESS is needed with PID_SMTP_HOST` (set
  `PGW_EMAIL_FROM_ADDRESS`), `PID_SMTP_USERNAME is set without PID_SMTP_PASSWORD`, or a
  signing key it cannot decrypt because `PID_KEK` or `PID_KEK_ID` changed. `id-db` must have
  exited with 0 first (`dc logs id-db`).
- **503 `identity_unavailable`** from the management API ("sign-in is not available right now
  (pacod-id); try again shortly"): the core's copy of pacod-id's feed is older than
  `PGW_ID_MAX_STALENESS` (60 seconds), or pacod-id does not answer on its internal listener.
  `dc ps id`, then `dc logs --tail 50 core`. Only the management API is affected; the data
  plane never asks pacod-id and keeps serving. It recovers by itself once pacod-id answers.
- **pacod-id's sign-in works, but the console asks for the password again and again**: the
  core refuses the console's token, so the console forgets it. The core, the console and the
  tester must run with the same `PGW_AUTH`: after changing it, run `dc up -d --wait`, which
  recreates all three, not `dc up -d ui` alone. Outside compose, the core's `PGW_ID_ISSUER`
  must be exactly the realm's issuer, `https://<PGW_ID_HOST>` (with the port, if Caddy is not
  on 443) without a path; compose and the Helm chart derive both from the same setting.
- **"No access yet"** after signing in: the account exists at pacod-id but holds no role of
  the gateway. A platform admin gives one (Platform, Access) or an organisation admin invites
  the address (section 14).
- **An admin token `pgwa_…` is refused** after the move: pacod-id checks these with the
  gateway's pepper, `PID_LEGACY_PEPPERS=gw:<PGW_KEY_PEPPER>`. Compose sets it from
  `PGW_KEY_PEPPER`; with Helm the chart does. It must be the pepper the tokens were made
  under. A `pgws_…` token (a session of the console's own login) is refused on purpose with
  `moved_to_identity`.
- **410 `moved_to_identity`** from the management API: the call is one pacod-id took over
  (signing in, passwords, reset and verification links, accepting invitations). The answer
  names the issuer; do it there.
- **A new organisation or workspace is unknown at pacod-id** (members or invitations of it
  fail): it waits in the identity outbox. The gauge `gw_identity_outbox_rows{state}` counts
  rows that wait (`pending`) and rows pacod-id refused for good (`parked`); the core logs a
  parked row with "pacod-id refused an identity outbox row for good". A newer change to the
  same organisation or workspace replaces a parked row, and a sweep at start and every five
  minutes queues what pacod-id does not show.
- **Mail from pacod-id does not arrive**: pacod-id sends it, not the core (section 13).

### A provider or model fails

Alerts: `GatewayErrorRateHigh`, `GatewayTimeoutsSpike`, `GatewayAllCallsFailing`.

1. Grafana, "Providers and models": is it one model, one provider, or everything?
2. Console, Requests, filter by the provider and status: the provider's own error message
   is stored with each failed call.
3. One provider, all calls, HTTP 401 or 403: the provider key is wrong or revoked. Rotate
   it (section 8).
4. HTTP 429 from the provider: its rate limit. In managed mode the gateway retries before
   the first byte and then reports the failure; proxy mode passes the 429 on. A 402
   `upstream_quota_exhausted` means the provider said the credential's quota or balance is
   used up (the codes are in the provider's quirk `error_rules`): it is not retried, and
   only topping up the provider account helps. A routing scheme that fails over on rate
   limits moves such calls to another model.
5. Timeouts: the provider is slow, or the timeout of the template is too short for the
   model. The gateway never retries a timeout, because the provider may have completed and
   billed the call.
6. Everything fails for every provider: check outbound connectivity and DNS from inside the
   stack. `dc exec caddy nslookup api.openai.com` must resolve, and
   `dc exec caddy wget -S --spider https://api.openai.com/ 2>&1 | head -3` must show an
   HTTP status line. Any status, even 401 or 404, proves that the provider is reachable.
7. A provider on a private address (a local model server) is refused unless it is listed
   in `PGW_EGRESS_ALLOW_PRIVATE`.
8. Calls succeed but cached or reasoning tokens are always zero for an OpenAI-compatible
   provider: it reports them somewhere else. The provider's quirk profile (Platform,
   Providers, the provider) names the places as dotted paths inside `usage`, tried in order:
   `cached_tokens_at`, `reasoning_tokens_at` and `cache_write_tokens_at`. `extra_params` lists
   the request members managed mode passes on to it. A change applies to the next call.
9. The request log shows `model_reported` different from the model asked for: the provider
   answered with another model. When that model is in the repository, the call is
   reported and priced as that model.
10. A tool loop fails on its second turn with a 400 from the provider: the provider wanted
   its earlier reasoning or thought signature back. Gemini 3 needs the signature of each
   function call, DeepSeek its reasoning on turns with tools, Kimi K3 always. The client must
   send the assistant message back as it received it (OpenAI dialect: with
   `extra_content` and `reasoning_content`; Anthropic dialect: with its `thinking` blocks).
   The provider's quirk `echo_reasoning` says when the gateway sends reasoning back.

### Requests are rejected

Alert: `GatewayRejectionSpike`.

The label `reason` says why. Grafana, "Gateway internals", panel "Rejections by reason".

- A rate limit or a quota: a workspace reached a limit. Console, the workspace, Limits,
  shows each limit and how much of it is used. Raise the limit there if that is intended.
  It takes effect with the next request. The alert `GatewayLimitNearlyUsed` warns before
  that: at four fifths of a limit that counts a whole workspace or organisation, and again
  when it is used up. Grafana, "Workspaces", panel "Share of each limit that is used".
  Limits per key and per end user have no alert; the console shows them.
- `gateway_overloaded`: the instance reached its cap of calls in flight. Add instances
  (section 9) or raise `[limits] max_in_flight` in the bootstrap file.
- Authentication failures: an application uses a revoked or mistyped gateway key.
- `own_credential_required` (403, proxy mode): a call that names no model the gateway can
  check (files, batches, a key set to blind relay, Ollama's housekeeping) reached a provider
  whose credential is the installation's shared one. It would reach other workspaces' files
  and batches there. Give the workspace or its organisation a credential of its own:
  Platform, Providers, the provider, Credentials, with the scope of that workspace or
  organisation. Model lists pass on any credential.
- `unsupported_media_type` (415, proxy mode): a model call was sent without the content type
  `application/json`, so the gateway could not read its model. Fix the client; `curl -d`
  without `-H 'content-type: application/json'` is the usual cause.
- `not_allowed` naming a provider tool (403): the workspace's allowed provider tools do not
  include it (web search, code execution, the MCP connector as `mcp`, …). The setting is
  checked in both modes and from every interface. Change it under the workspace's or the
  organisation's settings if the tool is wanted.
- `usage_required` (400, proxy mode): a streamed OpenAI-dialect call did not ask for usage
  (`stream_options.include_usage`), and the key requires it. A key without a usage policy of
  its own requires usage while an enforced token or spend limit counts its calls: one of its
  workspace or organisation that is not aimed at other keys, or one of its rate class; set
  the key's policy to `estimate` or `ignore` if that is not wanted, or
  give its template `request_stream_usage`.
- `not_allowed` naming an Anthropic beta feature (403): the installation's
  `anthropic_betas_allowed` setting (Platform, Settings) does not list the `anthropic-beta`
  value the caller sent. Unset allows every beta feature.
- HTTP 429 from the management API with "invalid admin tokens": an address presented more
  than 60 unknown admin tokens in a minute and is held off for a minute. Sessions, which the
  console uses, are not counted.
- HTTP 413 from Caddy: the body is larger than `PGW_MAX_BODY`. The core has its own limit
  (`[limits] max_body_bytes`, 32 MiB by default); raise both.

### Prices are missing or stale

Alerts: `GatewayPriceMissing`, `GatewayPriceStale`.

A call to a model without a price is settled without a cost, so spend reports and spend
caps undercount. Console, Platform, Pricing: enter the price, mark the model as free, or
approve the open proposal. Grafana, "Providers and models", panel "Calls settled without a
price", shows which models are affected and since when.

A price is stale when it was neither entered nor confirmed by the price job for more than
the job's "stale days" (30 by default; the core exports it as
`gw_price_stale_threshold_days`, and the alert rule follows it). Each
run of the job that finds the same figures at its source confirms the price, so a stale
price means one of three things:

- The job does not run or fails: see the last runs on the Pricing page and
  [section 12](#the-price-job).
- No source of the job covers the model: its runs report the model as "no price". Add a
  source, or enter the price by hand.
- The price was entered by hand. The job never touches such a price, and so never confirms
  it. Check it against the provider's price list and enter it again.

Grafana, "Providers and models", panel "Days since each price was checked".

A source that fails every run with a timeout or a certificate error while the page opens in a
browser can be a TLS-intercepting proxy on the way out: the analytics image does not trust its
certificate. Give the image the proxy's CA certificate (and the core's, for the providers), or
let the price job's hosts past the proxy.

### The spool grows or events are dropped

Alerts: `GatewaySpoolBacklog`, `GatewayAuditRecordsLost`.

The core writes every usage event to its spool and ships batches to the analytics service.
The spool grows when shipping fails.

1. `dc ps analytics clickhouse`: are both healthy?
2. `dc logs --tail 50 analytics`: ClickHouse errors, for example a full disk or a wrong
   password.
3. `dc logs --tail 50 core | grep -i ingest`: can the core reach the analytics service,
   and is `PGW_INTERNAL_TOKEN` the same on both?
4. Once shipping works again the spool empties by itself. Nothing has to be replayed by
   hand; ClickHouse removes duplicates.

When the spool reaches its cap (1 GiB for usage events), new events are dropped and
counted, and requests keep being served. Dropped events are gone: usage and cost for that
time are incomplete, limits are not affected.

A scope can choose to be refused instead, with `503` and the message "the event spool is
full": the setting `on_spool_full` (`keep_serving` or `fail_closed`) of a workspace, else of
its organisation (which can lock it), else the global settings, else `full_policy` in the
`[spool]` section of the configuration file. `/readyz` answers 503 while the spool is full
only when the global setting (or, without one, the configuration file) is `fail_closed`, so
an instance stays in rotation for the workspaces that keep being served.

Audit-mode records have their own spool (4 GiB). When it is full, calls that require a
record are refused with `503 audit_unavailable`; others proceed without a record.

A spool directory that takes no writes at all (a disk remounted read-only, lost
permissions) is another matter: the log says "the event spool cannot be written", and
`/readyz` answers 503 until a probe write succeeds again, so the load balancer sends calls
to the other instances. Fix the volume; the instance comes back by itself.

Do not delete the `spool` volume to "fix" a backlog. It holds the events that have not been
stored yet.

### The gateway delay is high

Alert: `GatewayDelayHigh`, and in the console "The gateway delay in … mode is above its
target".

The gateway delay is the time the gateway adds to a call, without the provider's time.
Targets: 2 ms in proxy mode and 8 ms in managed mode at the 99th percentile, by default.
They are global settings (`delay_targets`); the alert rule and the console follow them.

1. Grafana, "Gateway internals": which mode, and is the time spent before the provider
   call (stage `pre`) or after it (stage `post`)?
2. `docker stats`: is the core, or a gateway, at its CPU limit? Then raise the limit in
   the compose file or add instances (section 9).
3. Is audit mode switched on for a busy workspace? It holds complete bodies in memory.
4. Connection time and retry time are shown separately (stages `connect` and `retry`).
   They depend on the provider and the network and are not part of the delay.

### A configuration change does not arrive

Alert: `GatewayConfigurationLag`.

Every instance polls the configuration version in PostgreSQL about once a second. When it
changed, the instance reads the rows the change log names and builds a new snapshot; after a
restore, or when the log misses a version, it reads the whole configuration.

1. Console, System: which instance is behind?
2. `dc logs --tail 50 <that instance>` (on its machine for a gateway of `deploy/node/`): it
   cannot reach PostgreSQL, or it failed to build a snapshot from the new configuration. It
   keeps serving on the old one.
3. `dc ps postgres`. If PostgreSQL is down, traffic continues on the last snapshot and the
   management API cannot write.
4. Restarting the instance makes it load the current configuration from scratch.

### An instance id is in use twice

Alert: `GatewayInstanceIdConflict`, and in the console "Another process presented the instance
id ...".

Two processes were given the same `PGW_INSTANCE_ID`, or a copy of one spool directory. A
process that starts with the id of a running instance refuses to start and says so in its
log; the alert then shows for a minute. A process that already runs keeps serving, but its
figures do not show in the console and its events cannot be told apart from the other's.

1. Console, System: the instance marked "id in use twice", and its host.
2. Give each instance its own `PGW_INSTANCE_ID` and its own spool volume, and restart the
   one that was wrong.

### An instance stopped reporting

Console alert: "The instance ... stopped reporting".

An instance has not written its heartbeat for more than 15 seconds and did not sign off: it
crashed, was killed, or cannot reach PostgreSQL. Caddy stops sending it requests as soon as
its `/readyz` fails.

1. Is it running? `dc ps`, or `docker compose -f deploy/node/docker-compose.yml ps` on its
   machine.
2. If it runs, its log says why it cannot reach PostgreSQL (`dc logs --tail 50 <instance>`).
3. If it was removed on purpose, nothing is to be done: it is forgotten after a day.

### An instance has different secrets

Console alert: "The instance ... has a different PGW_KEY_PEPPER" or "... a different
key-encryption key".

The instance compares check values of the shared secrets, never the secrets themselves.
With another pepper it refuses every gateway key; with another key-encryption key it cannot
read the provider credentials. Copy `PGW_KEY_PEPPER`, `PGW_KEK`, `PGW_KEK_ID` and
`PGW_KEK_OLD` from the central `deploy/.env` into its settings and restart it.

### A test run fails or leaves objects behind

Runs are started under **Platform → Testing** (see [testing.md](testing.md)). A run's page shows
every step with what was sent and what came back.

- *The tester does not answer*: check `docker compose ps tester` and its log. The console
  starts without the tester; only the Testing page is affected.
- *The session has ended*: a run uses the session of the admin who started it. Signing out
  ends the run's management calls; start it again. With `PGW_AUTH=oidc` the tester exchanges
  that session for a delegated grant at pacod-id (`TESTER_ID_CLIENT_SECRET`), so a run
  outlasts the admin's five-minute token, and stops as cancelled once the session ends.
- *Left over* on a run: clean-up could not delete these paths, for example after the tester
  was stopped during a run (the run then shows as *interrupted*). Delete the scratch
  organisations named `test-<run>-<suite>` in the console: first their keys, templates,
  limits, grants and routing schemes, then the workspace, then the organisation.
- Mock mode keeps its `tester-mock-*` providers and their models on purpose; they are reused
  by every run. Load tests keep their `tester-load-mock-*` providers and models the same way.

### A load test fails or shows errors

Load tests are started under **Platform → Testing → Load** ([testing.md](testing.md),
section 9). The page shows the agents, every second, and warnings for figures to read with
care.

- *No load agents are set up*: the tester has no `TESTER_LOAD_AGENTS` or `TESTER_LOAD_TOKEN`.
  Add `docker-compose.load.yml` (section 9).
- *An agent is not reachable*: check `docker compose ps load-agent` and its log, or on another
  machine `make load-agent-up` there and the firewall in between. *Refused the tester's token*
  means the agent's `LOAD_AGENT_TOKEN` differs from the tester's `TESTER_LOAD_TOKEN`.
- *The call before the load answered 401 or 403*: the key is wrong or revoked, or does not
  allow the model or the mode. No load was made.
- *Mock target, the call before the load fails*: the core refuses the mock's private address.
  Add `load-mock` to `PGW_EGRESS_ALLOW_PRIVATE`; preflight warns about it.
- *503 during the test*: the gateway refused calls with `gateway_overloaded` (more than
  `max_in_flight` calls at once, section 9), or a provider was unavailable. The gateway's log
  says which.
- *429*: a rate limit or concurrency cap of the key, its rate class or the workspace. A live
  test's own key has a concurrency cap at the test's peak.
- *402*: a quota; a live test stops at its own call limit.
- *An agent used most of its CPUs, or started calls late*: the figures describe the agent,
  not the gateway. Use more agents, or agents on other machines.
- *Not started*: in calls-a-second mode, more calls were running than allowed; the target did
  not keep up with the rate.
- *Left over*: as for test runs (above), the scratch organisation is `load-<id>`.
- *Interrupted*: the tester stopped during the test. The agents stopped on their own when
  their lease ran out (30 seconds).

### PostgreSQL, ClickHouse or the disk

- PostgreSQL down: requests are served; limits are counted in memory and written when it
  returns, each instance enforcing only its own count meanwhile; the console cannot save
  changes, and an instance that restarts cannot start.
- ClickHouse or analytics down: requests are served; events wait in the spool; reports in
  the console answer 503.
- Disk full: check `docker system df -v` for the volumes. ClickHouse and Prometheus are
  the ones that grow. Prometheus is capped at 8 GB and `PROMETHEUS_RETENTION`.

## 11. Development stack

For work on the code, the override file builds the images from the working tree, adds a
mock provider and publishes the services on the host. It needs no `deploy/.env`: every
secret has a fixed, public development value. The production secrets in `deploy/.env` do not
change them, since the development volumes keep the passwords and keys they were made with
(compose reads `deploy/.env` for both stacks); other settings, such as provider API keys,
still come from it.

```sh
docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.dev.yml \
  -f deploy/docker-compose.load.yml -f deploy/docker-compose.load.dev.yml up -d --build --wait
```

`make dev-up` and `make dev-down` do the same. The development stack always has a load agent
and the load tests' mock (`docker-compose.load.dev.yml`).

| Service | Address on the host |
|---|---|
| Data plane | `http://localhost:18080` |
| Management API | `http://localhost:18081` |
| Core metrics | `http://localhost:19100/metrics` |
| Analytics | `http://localhost:18090` |
| Console | `http://localhost:13000` (through Caddy, so that the login works over HTTP) |
| PostgreSQL | `localhost:15433`, user `pacod_gw`, password `pacod_gw_dev_postgres` |
| ClickHouse | `http://localhost:18123`, user `pacod_gw`, password `pacod_gw_dev_clickhouse` |
| Grafana | `http://localhost:13001`, user `admin`, password `pacod-gw-dev` |
| Prometheus | `http://localhost:19090` |
| Mock provider | `http://localhost:19000`; inside the stack `http://gw-mock:9000` |
| Tester | `http://localhost:18095` (needs a platform admin's session token) |
| Load agent `local` | `http://localhost:18096` (token `dev-only-load-agent-token-0123456789abcdef`) |
| Load agent `remote`, with `make load-agent-dev-up` | `http://localhost:18196`; it reaches the stack at `host.docker.internal:18080` |
| Mock provider of load tests | `http://localhost:19001`; inside the stack `http://load-mock:9000` |
| pacod-id, sign-in pages and issuer | `http://127.0.0.1:13070` (through Caddy) |
| pacod-id, internal API | `http://127.0.0.1:18071` |
| pacod-id, metrics and probes | `http://127.0.0.1:19170/metrics` |
| Mailpit, pacod-id's mail | `http://127.0.0.1:18025` |

Console login: `admin@example.com`, password `pacod-gw-dev`. To send requests to the mock
provider, add a provider in the console with the base URL `http://gw-mock:9000`.

**Signing in through pacod-id.** pacod-id always runs in the development stack, built from
`vendor/pacod-id` with `deploy/pacod-id/pacod-id.dev.toml`. To have the console sign people
in through it:

```sh
PGW_AUTH=oidc make dev-up
```

Then open the console at `http://127.0.0.1:13000`, the only address pacod-id's development
configuration sends people back to; the console sends other addresses there first. Sign in
with `admin@example.com` and `pacod-gw-dev` again: in this mode the account is pacod-id's
first administrator (`PID_BOOTSTRAP_ADMIN_EMAIL`, `PID_BOOTSTRAP_ADMIN_PASSWORD`), a platform
admin of the gateway. pacod-id's mail (invitations, password resets, confirmations) always
goes to Mailpit, whatever `deploy/.env` says. With `DEV_HTTPS=1`, and for a development
stack on a machine of its own (below), use the console's own login: pacod-id's development
configuration knows only `http://127.0.0.1:13000`. `make dev-up` with `PGW_AUTH` unset
returns to the console's own login. The people of the two modes are separate until they are
moved (section 16).

The smoke and load tools sign in through pacod-id with `SMOKE_AUTH=oidc` (section 15), as the
development-only public client `gw-smoke`. How long the core keeps taking a token after its
session ended at pacod-id is measured, on this stack only, with:

```sh
cargo run -q -p pacod-gw-testkit --bin gw-smoke -- revocation   # 20 samples; --samples N
```

Each sample signs in, ends the session as a sign-out does and asks the core every 20 ms
until it refuses the token; it fails when the slowest sample takes more than 2 seconds, the
target of the identity concept. On 2026-10-13: 20 samples, p50 2 ms, p95 3 ms, max 3 ms. It
uses the core's development client secret to end sessions (`SMOKE_ID_SERVICE`,
`SMOKE_ID_SERVICE_SECRET`, `SMOKE_ID_INTERNAL_URL` point it elsewhere).

The ports are bound to `127.0.0.1`. Never use the override file on a machine that others
can reach. To run a binary directly instead of its container, see `.env.example` in the
repository root.

Stop it with `make dev-down`. To delete the data as well, add `-v` to the command above with
`down`.

**Ports 80 and 443.** `make dev-up DEV_HTTPS=1` adds `deploy/docker-compose.dev.https.yml`,
and Caddy then also serves the production host names on 127.0.0.1:

| Address | What |
|---|---|
| `https://console.localhost` | The console |
| `https://api.localhost` | The data plane (in the scale layout, balanced over the gateways) |
| `http://` on either name | Redirects to HTTPS |

The certificates come from Caddy's own certificate authority, whatever `deploy/.env` says, so
the browser warns once per name. To trust them, take the root certificate from
`docker compose -f deploy/docker-compose.yml exec caddy cat /data/caddy/pki/authorities/local/root.crt`,
or use `curl -k`. The links in account emails then point at `https://console.localhost`. All
other ports stay as in the table above, the console on 13000 included. Ports 80 and 443 must be
free: stop whatever holds them first, or start without `DEV_HTTPS=1`. `make dev-scale-up
DEV_HTTPS=1` does the same for the scale layout. Without the Makefile, add
`-f deploy/docker-compose.dev.https.yml` last, after `docker-compose.scale.dev.yml` if that
is used.

**On a machine of its own.** To run the development stack on another machine on your
network, say `10.0.0.150` with the DNS name `devbox.lan`, and use it from yours, start it with
`DEV_HTTPS=1` and give the HTTPS sites that machine's names. Plain HTTP is not enough: the
console's session cookie is `Secure`, and a browser keeps such a cookie from plain HTTP only
for `localhost` and `127.0.0.1`, so on `http://devbox.lan:13000` every sign-in ends on the
login page again. Put the settings into `deploy/.env` on that machine, which compose reads:

```sh
PGW_DEV_HTTPS_BIND=0.0.0.0          # ports 80 and 443 on the network; all others stay on 127.0.0.1
PGW_DEV_CONSOLE_HOST=devbox.lan     # https://devbox.lan is the console
PGW_DEV_API_HOST=10.0.0.150         # https://10.0.0.150 is the data plane
PGW_DEV_DEFAULT_SNI=10.0.0.150      # needed because a list holds an IP address
PGW_CONSOLE_URL=https://devbox.lan  # the console in account emails
```

and run `make dev-up DEV_HTTPS=1` there (`DEV_HTTPS` is a make variable and cannot go into
`deploy/.env`).

- `PGW_DEV_CONSOLE_HOST` and `PGW_DEV_API_HOST` each take one or more names or IP addresses,
  separated by spaces. An entry may appear in only one of them: one name cannot be both the
  console and the data plane. With a second DNS name, for example `api.devbox.lan`, the data
  plane can have that instead of the address.
- `PGW_DEV_DEFAULT_SNI` is needed only when a list holds an IP address, and then names that
  address. A client that connects to an address sends no server name, and without this
  setting Caddy does not know which certificate to present and ends the handshake.
- On your machine, trust Caddy's root certificate, from the `docker compose ... exec caddy
  cat ...` command above, run on the development machine. On macOS:
  `sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain root.crt`.
  The certificate authority lives in the volume `caddy-data` and stays the same until the
  volumes are deleted.
- `PGW_DEV_BIND=0.0.0.0` instead publishes every development port on the network:
  the data plane on 18080, PostgreSQL, ClickHouse, Grafana, the mock and the tester. They
  then use the public development secrets of the table above unless `deploy/.env` on that
  machine sets its own (names as in `deploy/.env.example`) before the first start. Only do
  this on a network you trust. Grafana then needs
  `PGW_DEV_GRAFANA_URL=http://devbox.lan:13001/`.

**Several instances.** `make dev-scale-up` starts the development stack in the scale layout
(`docker-compose.scale.yml` and `docker-compose.scale.dev.yml`): the management API alone in
`core`, and two gateways. The data plane on `http://localhost:18080` is then Caddy, balancing
over the gateways, so the smoke and load tools go through the balancer; the gateways answer
directly on 18082 and 18083 (metrics 19101 and 19102). The OTLP collector is published on
14318. `make dev-scale-down` stops it.

**A gateway on "another machine".** `make node-dev-up` starts `deploy/node/` with
`deploy/node/docker-compose.dev.yml` as its own compose project, `pacod-gw-node-dev`. It
shares no Docker network with the stack and reaches PostgreSQL, the analytics service and the
collector only through their published ports, as a gateway on another machine would. It
answers on 18180 (metrics 19180) and appears in the console as `node-1`. To include it:

1. Register the mock provider as `http://host.docker.internal:19000`: every gateway reaches
   it there, `gw-mock:9000` only the gateways of the stack.
2. Recreate Caddy with the node in its upstreams:
   `PGW_DATA_UPSTREAMS="gateway-1:8080 gateway-2:8080 host.docker.internal:18180" make dev-scale-up`.
3. For Prometheus, write `deploy/prometheus/targets/nodes.yml` with
   `host.docker.internal:19180` and the instance `node-1`.

`make node-dev-down` stops it.


## 12. Audit mode, the price job, moving a configuration

Three features that need a decision or a routine from the person who runs the stack.

### Audit mode

Audit mode stores complete requests and answers: what the application sent, what the
gateway sent to the provider on each attempt, what the provider answered, and what the
application received, with the time stamps of the call. It is meant for debugging and for
workspaces that must keep such a record. It is off unless somebody switches it on.

| Where | What can be set |
|---|---|
| Workspace (console: the workspace, Settings) | Mode: off, on, or "on request" (only calls that send the header `x-gw-audit: true` or `gateway.audit` in a managed-mode body). An end date, after which it switches itself off. A sample share. Retention in days (14 by default). Whether attachments are stored in full or as size and checksum. Whether a call may proceed without its record (best effort, the default) or is refused when the record cannot be written (required). |
| Template | A switch: as the workspace says, always on, or always off for calls through this template. The other settings come from the workspace. |
| Organisation | A rule for all its workspaces: forbid audit mode, or force it. |
| Platform (console: Platform, Settings) | The same rule for everything, and the largest body that is recorded (8 MiB by default; a longer body is cut and the record says so). |

What to know before switching it on for a busy workspace:

- The records hold prompts and answers, which may be personal data. Only workspace admins
  and auditors can read them, every read is written to the audit log (console: System,
  audit log, action `read_audit_record`), and the records of one end user or one session can
  be deleted (console: Insight, Audit records; API: `DELETE /api/v1/audit-records` with
  `workspace_id` and `end_user` or `session_id`). Provider keys and gateway keys are removed
  before a record is written.
- Records are stored in ClickHouse and removed there when their retention ends. They are
  part of the ClickHouse backup. Plan the volume: a record is about as large as the request
  and the answer together, stored once where the gateway did not change them.
- The calls of one trace or one session can be exported as JSON lines, oldest first
  (console: Insight, Requests, filtered by trace or session, "Export … as JSON lines"; API:
  `GET /api/v1/requests/export?trace_id=…` or `session_id=…`, with `audit=true` for the
  records). Each line holds a call's usage event and, where the caller may read audit
  records, its record. Every export is in the audit log, action `export_requests`.
- While a call runs, its bodies are held in memory, and the records wait in their own spool
  on the core's volume (4 GiB) until the analytics service has taken them. With the
  guarantee "required", calls are refused with `503 audit_unavailable` when that spool is
  full. The alert `GatewayAuditRecordsLost` covers both the full spool and lost records.

### The price job

The price job looks up the providers' prices on a schedule and keeps the model repository
current. It runs inside the analytics service, which asks the core for work every 30
seconds; only one instance runs it at a time. It is off until it is configured (console:
Platform, Pricing, or `PUT /api/v1/price-job`).

Requirements:

- `PGW_CORE_ADMIN_URL` of the analytics service points at the core's management API (set in
  the compose file and the Helm chart), and both share `PGW_INTERNAL_TOKEN`.
- The analytics service can reach the URLs of the job's sources. That is the one outbound
  connection the analytics service makes. Without outbound access, leave the job off and
  import a price file instead (console: Pricing, Import; `POST /api/v1/prices/import`).
- A source must be on a public address. A private, loopback or link-local one is refused
  when the job is saved and again when it is fetched, redirects included, unless the host
  or its range is in `PGW_EGRESS_ALLOW_PRIVATE` (the same list as for provider addresses;
  the compose file and the chart pass it to both services).
- For sources of the kind "page" (a provider's pricing page read by a model), the job names
  a template of the built-in `_system` workspace. The call goes through the gateway like any
  other and is billed to `_system`, where the gateway's own calls are. `_system` is created
  at start-up, may use every active or deprecated model, and cannot be deleted. An installation whose job
  named a template in another workspace has it copied into `_system` once, at the first start
  of this release. This path was tested against the mock provider only.

What a run does with what it finds, per model: a price entered by hand is left alone; the
same figures confirm the current price; a change within the bound (30 % by default) is
applied as a new price version; a larger change, and a model's first price, become a
proposal that waits for approval on the Pricing page. Nothing is ever changed backwards:
calls already settled keep the price they were settled with.

The job reads every rate a price has: input, output, cache reads and writes, reasoning,
per character (speech), per request and per provider tool call (a web search), and the
variants a provider bills apart under the tier its usage reports (`batch`, `priority`,
`flex`). From LiteLLM's list it also reads long-context tiers (Gemini 3.1 Pro above 200,000
input tokens) and off-peak hours (DeepSeek); so does a pricing page read by a model, which
needs a capable model in its template (a small one keeps the structure but mixes up rates).
LiteLLM files some providers' models under a prefix other than the gateway's provider name:
set the source's `key_prefix` (`zai` for GLM, `moonshot` for Kimi, `gemini`, `deepseek`). A rate or a variant the current price does not have cannot be measured as a
change, so it makes a proposal. After an upgrade to this release, the first run therefore
proposes prices for the models whose source now shows more rates than the price holds, for
example batch rates from LiteLLM's list; approve them once.

Activating a draft model asks for a run of that model alone, when the job is on and one of
its sources covers the model's provider. Such a run does not move the schedule; asking for
a full run, or a scheduled one, covers it.

Routine: look at the Pricing page when the console shows an alert for open proposals, a
missing price or a stale one. To see why a run did what it did: Pricing, Runs, or
`GET /api/v1/price-job/runs`; every model is listed with the outcome and the reason.

### Model discovery

A management instance asks every active provider for its model list, once a day by default
(Platform, Settings, `discovery_interval_hours`; 0 runs it only when asked) and whenever
someone presses Run now (Platform, Models, Discovery; `POST /api/v1/discovery/runs`). A model
the provider lists and the repository lacks becomes a proposal: Add as draft creates a draft
model, which still needs a price and an activation; Dismiss keeps it from being proposed
again. A proposal shows what the provider reports (context window, image input, reasoning)
where it reports it; Add as draft chooses the capability preset from the model's id and these
reports, so check it on the draft. The model is named `<provider>/<id>`, or by the provider's
id alone when you choose so above the list (`naming: bare`; PACOD names Gemini models that
way). A repository model the provider no longer lists is flagged; nothing is retired by
itself. Platform, Providers, Test asks one provider for its list at once, which tests its
address and its shared credential. Each proposal shows the capability preset and the kind
(chat, embedding, speech) the gateway suggests; change them before adding the draft where they
are wrong. Gemini's speech models (`-tts`) are proposed as speech models, which managed mode
does not serve on Gemini: use proxy mode for them. Vertex AI has no model list: discovery
skips its providers.

### Holiday calendars

Prices with off-peak hours apply all day on public holidays where the provider says so
(DeepSeek, on Chinese public holidays). The dates are in the platform settings (Platform,
Settings, Holiday calendars; `holiday_calendars` in `PUT /api/v1/settings`): a calendar has a
name, a time zone, a source and its dates. A provider names its calendar in `holidays`. The
release ships `cn` with China's 2026 holidays (the State Council's notice of November 2025).
Countries publish the next year's dates late in the year: add them when they appear. Monitoring
raises an alert when a linked calendar is missing, has no dates for this year, or, from
1 December, none for the next. Make-up working days are not listed.

### Retiring models at their sunset

Deprecating a model can name a sunset and "retire it at the sunset". A management instance
then retires it once the sunset has passed; calls follow its successor where grants allow.
The audit log records the transition with the actor `sunset`.

### Rate classes

Platform, Rate classes defines throttles once (calls and tokens per minute, calls at once)
that API keys name, instead of repeating numbers on every key. Each key in a class has its
own counters; the key's and the workspace's limits apply too.

### Limits

A workspace limit counts the whole workspace, each key or each end user. Its filter can
narrow it to some models, templates, tags or API keys: a limit for one key is a workspace
limit with that key in "Only these API keys" (the key list links to its limits). Token
limits can count all tokens, input, output, cache reads, cache writes or reasoning tokens.
An organisation's caps are set by platform admins, who can also reset them; a reset reaches
every instance within a few seconds.

### Anthropic files and message batches

Anthropic's SDK reaches its Files and Message Batches APIs through the gateway as it reaches
`/v1/messages`: same base URL, a gateway key in `x-api-key`. The gateway answers these paths
only when `anthropic-version` is sent (the SDK always sends it); OpenAI's file API is not
served in managed mode and goes through proxy mode on a credential of the workspace's own.

- **Which provider and credential.** A batch goes to the provider of its items' models.
  Files go to the workspace's Anthropic provider; when the workspace has models at several,
  the caller names one with `x-gw-provider: <provider name>`. The credential is the one a
  message would use: the workspace's, else the organisation's, else the shared one. Later
  calls about a file or batch use the credential that created it, so an object made on the
  shared credential stays reachable after the workspace gets one of its own.
- **Who sees what.** Every file and batch belongs to the workspace whose key created it.
  Lists show only the workspace's own, and another workspace's id answers `404`. A message
  (managed or proxy mode) that names another workspace's file id is refused with `404
  file_not_found`; on the shared credential so is a file id the gateway has no record of.
  On a workspace's or organisation's own credential an unrecorded id passes, since it may
  have been uploaded there directly.
- **Batches are governed per item.** Each item is resolved, merged and filtered like a single
  message: templates, grants, the capability filter and provider tools apply. Items must not
  stream, and all of them must go to one Anthropic provider; there is no failover. A
  batch's `results_url` names the gateway's `/v1/messages/batches/{id}/results`, so the SDK
  fetches results through the gateway.
- **Accounting.** Each item counts as one request against the limits when the batch is
  created, and the batch is refused if the items do not fit: `402` for a quota, `429` for a
  per-minute limit. The cost uses the
  model's price in effect at creation, at its `batch` variant (or the base rates where the
  price has none). The admin role's upkeep task (every 30 seconds) asks Anthropic about each
  open batch at most once a minute; once a batch has ended, it reads the results and writes
  one usage event per item, dated when the batch ended and with the batch id as trace id. Succeeded items carry their usage
  and cost, and token and spend limits count them then, in the window of the settlement.
  Errored, cancelled and expired items are recorded without usage or cost (error types
  `batch_errored`, `batch_canceled`, `batch_expired`). A batch can be deleted only after it is accounted (`409
  batch_not_settled` before).
- **In the console.** A workspace's Provider objects page lists its files and batches,
  deleted ones included, with the key that created each, its status and, for batches, the
  accounted cost in the reporting currency.
- **With Helm,** the admin role's spool is an `emptyDir`. Batch events are written there and
  shipped within seconds; a pod deleted before that loses the unshipped events, and the
  batch stays accounted without them.

### Currencies

Costs are computed in the currency of each model's price. When a call settles, its cost is
also converted into the reporting currency of the global settings with the exchange rate
held there (`exchange_rates`, one `EUR: 1.08` per line in the console: what one unit is
worth in the reporting currency). Reports, the overview, the console's totals and spend
limits use the converted cost; the request log, the `x-gw-cost` header and the
`gw_cost_micros_total` metric keep the price's currency.

A price in a currency without a rate is counted unconverted, and the alert "No exchange
rate for …" names the currency and its models. Add the rate; calls settled before keep
their unconverted figure. Events stored before this release have no converted cost; reports
count them as recorded. Rates are not fetched: update them when they move enough to
matter.

### Templates as files

A template's Export gives a file that Import (on the templates list) creates again, in
another workspace or another installation; its models are named by their ids and must be
granted there. Every version of a template is kept: the template page lists all of them.

### Changing configuration through the API

- **Versions.** Every object carries a `version`, the global settings, the price job and an
  organisation's routing assignment included. An update (`PUT`) must send the version it
  read: without one the answer is `428 version_required`, with an older one `409`. Read
  the object again and repeat the change. Changing a member's roles replaces their set of
  roles and takes no version.
- **Applied where.** Every change answers with two headers: `x-gw-config-version`, the
  configuration version it produced, and `x-gw-applied`, on how many of the running
  instances it is applied (`1/3`). `GET /api/v1/system/config-versions/{version}` reports
  the count later. The console shows "applied on n of m instances" while it is short.
- **Trying a change first.** `?dry_run=true` on the updates and deletes of the resources
  that are plain objects (organisations, workspaces, keys, providers, models, grants,
  templates, limits and the like), on revoking a key, on a model's state change and on an
  import runs the change in a transaction that is rolled back, so every check applies. The
  operations that offer it list `dry_run` in the OpenAPI document; any other change asked
  for with `dry_run` is refused (`422`) rather than made. The answer has the object as it would be, `applied: false`, and for
  grants and providers the effects (for example the templates and keys a deleted grant
  would stop).
- **Lists.** `GET` on a collection of plain objects takes `limit` and `cursor` (the
  answer's `next_cursor`), `sort` (`name`, `created_at`, `updated_at`), `order` (`asc` or
  `desc`), `q` (part of the name) and `status`. The model list is paged too, 500 by
  default. Credentials, files, batches and the audit log take `limit` and `cursor` only;
  prices, price proposals and runs, discovery runs and proposals and configuration
  versions are answered whole.
- **Following changes.** `GET /api/v1/events` is a server-sent event stream: `config` for
  every change the caller may read; platform roles also get `job` (price and discovery
  runs starting and finishing) and `health` (a model's success rate crossed 90 % or 50 %
  over the last five minutes). Job events come from the management instance that ran the
  job; with several management instances, a stream on another one does not see them.

### Moving a configuration

`GET /api/v1/system/config-export` (console: Platform, System) returns the configuration
as one JSON document: settings, rate classes, providers, models with their prices,
organisations, workspaces, grants, templates, limits and the price job. A template comes
with the 50 newest of its versions; older ones stay behind. It is for moving a
configuration from a test installation to production, and for keeping it under version
control. It is not a backup (section 7 is): it leaves out everything secret and everything
personal on purpose.

| Not in the export | After an import |
|---|---|
| Provider credentials | Enter each provider's key again (Platform, Providers, Credentials). Until then calls to that provider fail. |
| Gateway keys | Create new keys per workspace and hand them to the applications. |
| Users, admin tokens, role bindings | Create them again. With `PGW_AUTH=oidc` they are pacod-id's and not part of the configuration; organisations and workspaces an import creates are registered at pacod-id as API changes are. |
| Usage, the audit log, limit counters | Stay where they were. |

To import, a platform admin sends the document to
`POST /api/v1/system/config-import?dry_run=true` first. The answer lists what would be
created, updated and left unchanged, and what is refused with the reason. Then the same
call without `dry_run`. An import is all or nothing: if a single object is refused,
nothing is applied. Objects keep their ids, so an import into the installation the export
came from updates in place, and objects that exist only in the target are left alone
(an import never deletes). The import that is applied is written to the audit log; a dry
run, and an import that was refused, are not. An import checks that every document is
valid for its kind and that names are unique, not everything a single change through the
API checks (a provider URL against the egress rules at save time, a routing scheme's
rules, a template's models); an object the gateway cannot use is left out of the
configuration and shows in System status and as a Monitoring alert, and provider URLs are
still checked on every connection.

The console does both (Platform, System, Configuration). From a script, the management API
has to be reachable: section 4, "the management API from outside", or call `http://core:8081`
from inside the network.

```sh
# The token of a platform admin (console: Platform, Access, Admin tokens).
curl -fsS -H "Authorization: Bearer $ADMIN_TOKEN" \
  https://<API host>/api/v1/system/config-export > config.json
curl -fsS -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  --data-binary @config.json \
  'https://<API host of the target>/api/v1/system/config-import?dry_run=true'
```

## 13. Account emails

Who sends the emails about accounts depends on `PGW_AUTH`. With `local`, the default, the core
sends them, as the rest of this section describes. With `oidc` pacod-id sends them and the
core sends none; moving to pacod-id is section 16.

| | `PGW_AUTH=local` | `PGW_AUTH=oidc` |
|---|---|---|
| Sent by | the core | pacod-id (the `id` service) |
| What | email verification, password reset, invitations (section 14) | the same, and pacod-id's own: password or address changed, a new sign-in on another device, "you already have an account", "we signed you out everywhere" |
| Settings | `PGW_SMTP_*`, `PGW_EMAIL_*` and `PGW_CONSOLE_URL` in `deploy/.env` | the same `PGW_SMTP_*` and `PGW_EMAIL_*`, which compose passes to pacod-id as `PID_SMTP_*` and `PID_EMAIL_*` |
| Links lead to | the console, `PGW_CONSOLE_URL` | pacod-id's pages on `PGW_ID_HOST` |
| Lifetimes | global settings `verification_link_hours`, `reset_link_minutes`, `invitation_link_days`, `session_hours`, `remember_me_days` | the realm in `deploy/pacod-id/pacod-id.toml`: `verification_link_hours` (24), `reset_link_minutes` (60), `invitation_days` (7), `session_hours` (12), `remember_me_days` (7) |
| When a mail does not arrive | below | pacod-id's log says "a mail was not sent"; `vendor/pacod-id/docs/runbook.md`, section 13 |

The mail settings are set once and serve the core, pacod-id and the alert mail (section 10).
pacod-id is stricter about them: with `PGW_SMTP_HOST` set it needs `PGW_EMAIL_FROM_ADDRESS`
(the core falls back to the user name) and refuses `PGW_SMTP_USERNAME` without
`PGW_SMTP_PASSWORD`. Either stops it at start, and the core with it (section 6).
`email-check` (below) checks the settings as the core reads them, not as pacod-id does.
pacod-id does not wait for the server: a mail goes into its outbox and is tried again for
about eight hours, so a sending failure never shows in the console. Without `PGW_SMTP_HOST`
pacod-id sends nothing and asks no one to confirm an address. In the development stack its
mail always goes to Mailpit (section 11).

### With the console's own login

With an SMTP server configured, the core sends two kinds of email:

- **Email verification.** A user created by an admin, or whose address an admin changes,
  confirms the address before the next login. Accounts that existed before email was set up
  count as confirmed, and so does an admin who changes their own address.
- **Password reset.** "Forgot password?" on the login page, or "Send password reset" in the
  user list, sends a link to choose a new password. Using it signs the user out everywhere.

Each link works once and for a limited time (global settings: `verification_link_hours`,
default 24, and `reset_link_minutes`, default 60). Only the SHA-256 of a link's token is
stored. The login page answers a reset request the same way whether or not the account
exists, and sends at most three links per address and purpose within 15 minutes. "Keep me
logged in" at login makes the session last `remember_me_days` (default 30) instead of
`session_hours`.

The settings are in `deploy/.env` (`deploy/.env.example` explains each one):
`PGW_SMTP_HOST`, `PGW_SMTP_PORT`, `PGW_SMTP_TLS` (`starttls`, `tls` or `none`),
`PGW_SMTP_USERNAME`, `PGW_SMTP_PASSWORD`, `PGW_EMAIL_FROM_ADDRESS`, `PGW_EMAIL_FROM_NAME`, and
`PGW_CONSOLE_URL`, the console's address for the links. The core refuses to start when the
SMTP host is set without `PGW_CONSOLE_URL`; it never builds a link from a request's `Host`
header.

Check the settings without sending anything:

```sh
docker compose -f deploy/docker-compose.yml run --rm --no-deps core email-check
# and with a test message:
docker compose -f deploy/docker-compose.yml run --rm --no-deps core email-check --send-to you@example.com
```

When an email does not arrive:

- The core logs `account email not sent` with the user id and the server's answer. "Send
  verification" and "Send password reset" in the user list show the answer directly.
- Look in the recipient's spam folder. The sender address must be one the SMTP account may
  send as, or the provider rewrites or drops the message.
- Gmail and other providers that use two-factor sign-in need an app password, not the
  account password.
- If email cannot be fixed at once, a platform admin can mark an address as confirmed, or
  give the user a new password by editing the user.

### Links in the logs

The links carry their token in the URL. Caddy removes the `token` and `next` parameters
(after signing in, `next` can lead back to an invitation link) and the `Referer` header
from its logs. On the console's host and pacod-id's it also removes the sign-in's `code` and
`state`, and the `Location` header of redirects, which carry the same values; on pacod-id's
host also `id_token_hint`, `login_hint` and `flow`. A proxy in front of Caddy should do the
same.

## 14. Members and invitations

An organisation's admins manage who works in it, on the organisation's **Members** page or
with `/api/v1/orgs/{org}/members` and `/api/v1/orgs/{org}/invitations`. They give a member
one role in the organisation or in one of its workspaces, change it, and remove members. A
change that would leave the organisation without an organisation admin is refused, unless a
platform admin makes it. Workspace admins see their workspace's members but do not change
them. Platform admins can still bind any role directly under **Platform → Access**.

The pages and the paths are the same in both modes. With `PGW_AUTH=local` the core keeps the
members and invitations itself, as described below. With `PGW_AUTH=oidc` they are pacod-id's,
and the core answers these paths from pacod-id ("With pacod-id" below).

New people arrive by invitation, also when they already have an account: the inviter names
an email address and a role, never a user id, and learns nothing about which accounts exist.

- **With email configured** (section 13), the link goes to the address. Someone new chooses
  a display name and a password on the page it opens, which also confirms the address.
  Someone with an account signs in and accepts there, or under "Invitations for you" on the
  dashboard.
- **Without email**, or when sending fails, the console shows the link once, to copy and
  hand on. Whoever has the link can accept it, so send it only to the person invited. Where
  the installation is set up to send email, an account created from such a link still
  confirms its address by email before the first login; while sending fails, a platform
  admin marks the address confirmed in the user list (Platform → Access → Users).

Accepting gives the invited role in place of any role the person had in the same place: one
role per place, as on the Members page. An invitation is valid for `invitation_link_days`
(global settings, default 7) and works once. Inviting the same address to the same place again replaces the open invitation and
its link; "Resend" gives a new link; "Revoke" withdraws it. Expired invitations stay listed
for 30 days. Deleting a workspace or an organisation withdraws its invitations and removes
its members' roles. Only the SHA-256 of a link's token is stored; inviting, accepting,
declining and revoking are written to the audit log.

### With pacod-id

With `PGW_AUTH=oidc` the members of an organisation or workspace are pacod-id's members of the
scope `organisation:<org>` or `gw.workspace:<ws>`, and the invitations are pacod-id's. The
console and scripts keep calling the core with the same paths and get the same shapes. The
core checks the caller's roles as before, then calls pacod-id's admin API with the caller's
own token, exchanged for one for pacod-id, so pacod-id's rules apply on top. Every change is
written to the gateway's audit log, in the organisation's part, as before.

- **Accepting** happens on pacod-id's pages: the invitation's link leads there, and the
  console's dashboard links to the open invitations on pacod-id's account pages.
  Someone new creates the account there. Old links of the console's own invitations
  (`/invite?token=…`) are forwarded to pacod-id, which knows them after the move (section 16).
- **Mail** is pacod-id's (section 13). With SMTP the link is never shown in the console: the
  mail waits in pacod-id's outbox while the server fails, and "Resend" sends a new one.
  Without SMTP the console shows the link once, as above.
- **Lifetime**: the realm's `invitation_days` (7 by default) in
  `deploy/pacod-id/pacod-id.toml`. The global setting `invitation_link_days` is ignored.
- **Organisations and workspaces** reach pacod-id through the core's identity outbox, within
  seconds of their creation. Inviting into one pacod-id does not know yet delivers it on the
  spot, so an organisation can be created and its admin invited at once.
- **One difference in answers:** a member `PUT` with `role: null` for someone who is not a
  member answers 204, where local mode answers 404.

## 15. Running with make

The `Makefile` in the repository root wraps the commands of this runbook. Run every target
from the repository root, as `make <target>`. The targets that start services need Docker
with the compose plugin. The development targets also need Rust (`cargo`), Node.js (`npm`,
with `npm ci` done once in `ui/`) and `uv` for the tester.

### Running an installation

| Target | What it does | Details |
|---|---|---|
| `make images` | Builds the eight images (the core, analytics, the console, the tester, the minimal gateway, the load agent, the mock provider of load tests and pacod-id), tagged `0.1.0`. `make images VERSION=0.2.0` builds another tag; it must equal `PGW_VERSION` in `deploy/.env`. | Section 4 |
| `make up`, `make down` | Starts or stops the stack with one core. | Sections 1 and 4 |
| `make scale-up`, `make scale-down` | The same with the management API on its own and two gateways. | Section 9 |
| `make hub-up`, `make hub-down` | On the central host when gateways on other machines join: the scale layout plus the hub file. | Section 9 |
| `make node-up`, `make node-down` | On another machine: one gateway that joins the central host. | Section 9 |
| `LOAD_TEST=1` with `up`, `scale-up` or `hub-up` (and the matching `down`) | Adds `docker-compose.load.yml`: a load agent and the load tests' mock in the stack. | Section 9 |
| `make load-agent-up`, `make load-agent-down` | On another machine: a load agent for the central tester. | Section 9 |
| `make backup` | Backs up PostgreSQL and ClickHouse (`deploy/scripts/backup.sh`). | Section 7 |
| `make lite-up`, `make lite-down`, `make lite-check` | The minimal gateway, configured by `deploy/lite/.env`; `lite-check` shows what it reads, without secrets. | Section 17 |

The targets read their settings from `deploy/.env`; `node-up` and `node-down` read them from
`deploy/node/.env`, `load-agent-up` and `load-agent-down` from `deploy/load-agent/.env`. `up` waits until every service is healthy and fails if one does not
become healthy. `down` stops and removes the containers and keeps the volumes, so the data
stays. Stop a layout with the target that started it: `make down` does not know the gateways
of the scale layout and leaves them running.

A first deployment with make, after section 3:

```sh
cp deploy/.env.example deploy/.env     # then fill in the secrets and host names (section 3)
make images
make up                                # or: make scale-up
```

An upgrade (section 6): `make images VERSION=<new>`, set `PGW_VERSION` in `deploy/.env`, and
run the same `up` target again. Compose replaces only the containers whose image or settings
changed. For a rolling upgrade of several gateways, follow section 9 instead.

### Development

| Target | What it does |
|---|---|
| `make dev-up`, `make dev-down` | The development stack (section 11): images built from the working tree, the mock providers, a load agent, ports between 10000 and 19999, no `deploy/.env` needed. `DEV_HTTPS=1` adds ports 80 and 443 with `https://console.localhost` and `https://api.localhost`. `PGW_AUTH=oidc make dev-up` signs people in through pacod-id. |
| `make dev-scale-up`, `make dev-scale-down` | The same in the scale layout: Caddy on 18080, the gateways on 18082 and 18083. `DEV_HTTPS=1` as above. |
| `make node-dev-up`, `make node-dev-down` | A gateway that plays another machine, next to the development stack (section 11). |
| `make load-agent-dev-up`, `make load-agent-dev-down` | A load agent that plays another machine (`remote`, 18196), next to the development stack. |
| `make lite-dev-up`, `make lite-dev-down` | The minimal gateway built from the working tree, with the mock provider: 18280, mock 19280 (section 17). |
| `make run-lite` | The minimal gateway on the host, configured by `LITE_ENV` (default `deploy/lite/.env`). |
| `make test-services` | The PostgreSQL (25433) and ClickHouse (28123) containers the tests use. |
| `make run-core`, `make run-analytics` | The core or the analytics service on the host with `cargo run`, configured by the environment. `scripts/dev-core.sh` is a ready-made variant of `run-core` (below). |
| `make ui-dev` | The console's development server on `http://localhost:5173`; it calls the management API named in `ui/.env` (`GATEWAY_API_URL`). |
| `make run-tester` | The tester on 28095, against the core of `scripts/dev-core.sh`. |
| `make build`, `make check` | Builds, or type-checks, the Rust workspace and the console. |

The quickest loop without building images runs the core and the console on the host:

```sh
make test-services
docker exec pgw-test-pg createdb -U postgres gw_ui   # once
scripts/dev-core.sh                                 # data plane 28080, management API 28081
make ui-dev                                         # in a second shell
```

`scripts/dev-core.sh` uses development secrets and the first admin `admin@example.test`
with the password `correct horse battery`.

### Checks and tests

| Target | What it does |
|---|---|
| `make test` | The Rust tests. They need `make test-services`. |
| `make verify` | Everything CI checks: formatting, lints, every test of the Rust workspace, the console and the tester, and the OpenAPI drift check. |
| `make deploy-check` | Static checks of `deploy/` and `.github/`: compose files, Caddyfiles, Prometheus configuration and rules, the collector, shell scripts, the Helm chart, workflows. Starts no service. |
| `make lint`, `make fmt` | Lints, or formats, the Rust code, the console and the tester. |
| `make openapi` | Writes the OpenAPI document and the console's generated client after a change to the management API. `make openapi-check` fails when they are out of date. |
| `make helm-rules` | Copies the alert rules into the Helm chart after a change to them. |
| `make helm-deps` | Packages the vendored pacod-id chart into the gateway's chart (`charts/`) after `vendor/pacod-id` changed. |
| `make tester-test` | The tester's lint, format check and tests. |
| `make deny` | Licences, banned crates, sources and advisories of the Rust dependencies, by `deny.toml`. Needs `cargo install cargo-deny --locked`. CI runs it too. |

### Against a running stack

| Target | What it does |
|---|---|
| `make smoke-mock` | The whole flow against the mock provider: providers, models, both modes, both dialects, limits, routing, audit mode, the price tracker, Anthropic files and batches, key revocation. |
| `make smoke-live` | A few tiny calls per provider with the keys in `.env` (a fraction of a cent), a tiny file and a one-item batch at Anthropic (the batch is cancelled at once), then a scan of the logs for those keys. |
| `make load` | Calls through the gateway against the mock, compared with calling the mock directly. `PROFILE=managed` for managed mode, `LOAD_ARGS="--requests 20000 --concurrency 500"` for more. The console's load tests do more: stages, rates, several agents, other gateways ([testing.md](testing.md), section 9). |
| `make lite-smoke` | The minimal gateway's development stack: calls through it (`gw-smoke lite`), then each call's line in its log and no key in it. |
| `make lite-live` | The minimal gateway on the host against OpenAI and Anthropic with the keys in `.env`; each call logged with a cost, no key in the log. |

They go to the development stack by default: the management API on 18081, the data plane on
18080, which is Caddy in the scale layout. `SMOKE_ADMIN_URL`, `SMOKE_DATA_URL` and
`SMOKE_MOCK_URL` point them elsewhere; `PGW_ADMIN_EMAIL` and `PGW_ADMIN_PASSWORD` are the
login. Against a stack started with `PGW_AUTH=oidc`, set `SMOKE_AUTH=oidc`: they then sign in
at pacod-id (`SMOKE_ID_ISSUER`, by default `http://127.0.0.1:13070`) as the development
client `gw-smoke`, which only the development stack has. With a gateway from `make node-dev-up` among the upstreams, set
`SMOKE_MOCK_URL=http://host.docker.internal:19000`: the node cannot reach the mock under its
name in the stack.

## 16. Signing in through pacod-id

pacod-id is a separate OpenID Connect provider for the PACOD products. It is vendored in
`vendor/pacod-id` (the commit is in `vendor/pacod-id/VERSION`), released with the gateway
under the same version, and has its own runbook, `vendor/pacod-id/docs/runbook.md`. The
design is [identity-concept.md](identity-concept.md). With `PGW_AUTH=oidc` people sign in to
the console at pacod-id, and pacod-id holds their accounts, roles and admin tokens. With
`PGW_AUTH=local`, the default, the console keeps its own login and pacod-id runs unused,
ready to take over the gateway's people. The top of this document says what was exercised.

### What changes with `PGW_AUTH=oidc`

| | `PGW_AUTH=local` | `PGW_AUTH=oidc` |
|---|---|---|
| Signing in | The console's login page; the core's sessions (`pgws_…`). | pacod-id's pages on `PGW_ID_HOST`. The console is a client of pacod-id that keeps its tokens on the server side, sealed in one cookie (`pgw_session`, `__Host-pgw_session` on https). `/auth/login`, `/auth/callback` and `/auth/logout` are the console's paths for it. |
| Old pages | – | `/login`, `/forgot-password`, `/reset-password`, `/verify-email` and the account's password page lead to pacod-id; old invitation links (`/invite?token=…`) are forwarded there. The account menu leads to the account at pacod-id. |
| People, roles, admin tokens | The core's configuration. | pacod-id. The console's users, role bindings, admin tokens, members and invitations pages keep calling the same paths of the core, which answers from pacod-id (section 14). An admin token is a pacod-id service account with one `pidt_…` token. |
| Tokens the management API takes | `pgws_…`, `pgwa_…` | pacod-id's access tokens (five minutes, renewed by the console), `pidt_…` service tokens and imported `pgwa_…` admin tokens. `pgws_…` is refused (`moved_to_identity`). |
| Signing out, disabling, role changes | At once. | Within two seconds on every management instance, through pacod-id's feed of changes, which each instance follows. Open consoles hear of role changes on their event stream. |
| Endpoints pacod-id took over | – | `POST /api/v1/session`, the password, reset and verification endpoints and accepting invitations answer 410 `moved_to_identity` with the issuer. `DELETE /api/v1/session` ends the session at pacod-id. |
| First administrator | `PGW_ADMIN_EMAIL`, `PGW_ADMIN_PASSWORD` | `PID_BOOTSTRAP_ADMIN_EMAIL`, `PID_BOOTSTRAP_ADMIN_PASSWORD`; the core ignores `PGW_ADMIN_*` with a warning. |
| Account emails | The core. | pacod-id (section 13). |
| The data plane | Unchanged. | Unchanged: gateway keys never touch pacod-id, and the data plane keeps serving while pacod-id is down. |

In OIDC mode the core also:

- pushes the gateway's roles (`authz/gw.yaml`) to pacod-id at start, until pacod-id takes them;
- answers management calls with 503 `identity_unavailable` while its copy of pacod-id's feed
  is older than `PGW_ID_MAX_STALENESS` (60 seconds), or while pacod-id does not answer;
- sends every created, changed or deleted organisation and workspace to pacod-id through an
  outbox (the table `idp_outbox`, written in the same transaction as the change), so that
  roles can be held there; the gauge `gw_identity_outbox_rows{state}` counts the rows that
  wait (`pending`) and those pacod-id refused for good (`parked`);
- forgets an account pacod-id deleted (below, "Deleted accounts and erasure").

The tester follows the same switch: with `PGW_AUTH=oidc` a test run works with a delegated
grant of the admin's session (section 10, "A test run fails or leaves objects behind").

### Settings and secrets

All in `deploy/.env`; `deploy/.env.example` explains each. Preflight requires the secrets in
both modes, because pacod-id always runs.

| Variable | Read by | What it is | Generate with |
|---|---|---|---|
| `PGW_AUTH` | core, console, tester | `local` or `oidc`. All three must agree; compose gives them the same value. | – |
| `PGW_ID_HOST` | Caddy, pacod-id; the core and the console as the issuer | pacod-id's DNS name. The issuer is `https://<PGW_ID_HOST>`. | – |
| `PID_DB_PASSWORD` | `id-db`, pacod-id | The PostgreSQL role `pacod_id`. Letters, digits and `. _ ~ -` only. | `openssl rand -hex 24` |
| `PID_KEK`, `PID_KEK_ID`, `PID_KEK_OLD` | pacod-id | pacod-id's key-encryption key: its signing keys and queued mails are sealed under it. Keep a copy apart from the backups (section 3). Rotation: pacod-id's runbook, section 11. | `openssl rand -base64 32` |
| `PID_TOKEN_PEPPER` | pacod-id | HMAC key of client secrets, `pidt_…` tokens and known devices. Keep it with `PID_KEK`. | `openssl rand -hex 32` |
| `PGW_ID_CLIENT_SECRET` | pacod-id, core | The core's client, `pacod-gw-core`. | `openssl rand -hex 32` |
| `PGW_UI_CLIENT_SECRET` | pacod-id, console | The console's client, `pacod-gw-console`. | `openssl rand -hex 32` |
| `TESTER_ID_CLIENT_SECRET` | pacod-id, tester | The tester's client, `pacod-gw-tester`. | `openssl rand -hex 32` |
| `PGW_UI_SESSION_SECRET` | console | Seals the console's session cookie. Changing it signs everyone out of the console. | `openssl rand -hex 32` |
| `PID_BOOTSTRAP_ADMIN_EMAIL`, `PID_BOOTSTRAP_ADMIN_PASSWORD` | pacod-id | The first administrator of a new installation (below). Leave empty when moving a gateway's people. | `openssl rand -base64 18` (at least 12 characters) |
| `PGW_KEY_PEPPER` | also pacod-id, as `PID_LEGACY_PEPPERS=gw:<value>` | Checks the imported `pgwa_…` admin tokens. | (existing) |
| `PGW_SMTP_*`, `PGW_EMAIL_*` | also pacod-id, as `PID_SMTP_*`, `PID_EMAIL_*` | Its mail (section 13). | (existing) |

Compose sets the rest itself: `PGW_ID_ISSUER=https://<PGW_ID_HOST>` and
`PGW_ID_INTERNAL_URL=http://id:8071` for the core, the console and the tester, and the
console's `PGW_CONSOLE_URL=https://<PGW_CONSOLE_HOST>`, where pacod-id sends people back.
When Caddy is published on another port than 443 (`PGW_HTTPS_PORT`), set `PGW_ID_ISSUER` and
`PGW_CONSOLE_URL` in `deploy/.env` to those addresses with the port
(`https://id.example.com:8443`); the preflight check refuses them when they name other hosts
than `PGW_ID_HOST` and `PGW_CONSOLE_HOST`, or carry a path.
Outside compose these must be set by hand. The client ids (`PGW_ID_CLIENT_ID`,
`PGW_UI_CLIENT_ID`, `TESTER_ID_CLIENT_ID`) and the audience (`PGW_ID_AUDIENCE`, `pacod-gw`)
have defaults that match `deploy/pacod-id/pacod-id.toml`; `PGW_ID_MAX_STALENESS` is 60
seconds unless set.

pacod-id's own configuration is `deploy/pacod-id/pacod-id.toml`: the realm `gateway` and its
three clients, with the addresses taken from `PGW_ID_ISSUER` and `PGW_CONSOLE_URL` as above. The
realm's fields set the password rules and the lifetimes of sessions and links
(`session_hours` 12, `remember_me_days` 7, `verification_link_hours` 24,
`reset_link_minutes` 60, `invitation_days` 7 by default; pacod-id's runbook, section 3). In
OIDC mode they replace the gateway's global settings of the same purpose, which have no
effect then. Check a changed file with `dc run --rm --no-deps id check-config`, then
`dc restart id`.

### A new installation

To sign people in through pacod-id from the first start:

1. Follow section 4 up to step 5, and set in `deploy/.env`:

   ```sh
   PGW_AUTH=oidc
   PID_BOOTSTRAP_ADMIN_EMAIL=you@example.com
   PID_BOOTSTRAP_ADMIN_PASSWORD=<openssl rand -base64 18>
   ```

   `PGW_ADMIN_EMAIL` and `PGW_ADMIN_PASSWORD` are not needed. pacod-id gives the account the
   gateway's role `gw.platform_admin`. To let it administer pacod-id itself too (its pages
   under `https://<PGW_ID_HOST>/admin`), also set
   `PID_BOOTSTRAP_ADMIN_ROLES=id.admin,gw.platform_admin`.
2. `dc up -d --wait`. `dc logs id` says "created the bootstrap admin".
3. Open `https://<console host>/`. It leads to pacod-id: sign in with the address and the
   password, and change the password at `https://<PGW_ID_HOST>/account/security`.
4. Empty both `PID_BOOTSTRAP_ADMIN_*` in `deploy/.env` and run `dc up -d --wait id`.
   Leaving the address set without a password stops pacod-id's start if the account is ever
   deleted.

Platform admins then manage people on the console's users page, and organisation admins
invite their members (section 14).

### Moving an existing gateway's people

A gateway that ran with `PGW_AUTH=local` moves its users, admin tokens, role bindings and
open invitations to pacod-id once. Ids are kept, so the audit log stays linked; passwords,
`pgwa_…` admin tokens and invitation links keep working. Everyone signs in once more
afterwards. Until the last step the gateway can return to its own login.

The steps below were not run on a compose stack; the end-to-end test
`crates/pacod-gw-core/tests/oidc_migration.rs` covers the same sequence. pacod-id's side is
its runbook's section 19.

**Before you start:**

- The release with pacod-id runs with `PGW_AUTH=local`, and `dc ps` shows `id` healthy
  (section 6).
- `PID_BOOTSTRAP_ADMIN_EMAIL` and `PID_BOOTSTRAP_ADMIN_PASSWORD` are empty, and were empty at
  every start of `id`. An account pacod-id already has under one of the gateway's addresses
  is a conflict, and the import is refused.
- Make a backup (section 7).
- Agree a quiet period: from the export until the switch, nobody changes users, roles, admin
  tokens or invitations. The last step refuses what changed after the export.
- With a PostgreSQL that the compose file does not run, create pacod-id's role and database
  first, as a superuser and with psql 15 or newer (compose's `id-db` does this at every
  start). The passwords go in the environment, not on the command line:

  ```sh
  PGHOST=HOST PGUSER=pacod_gw PGDATABASE=pacod_gw PGPASSWORD=... PID_DB_PASSWORD=... \
    sh deploy/scripts/create-id-role.sh
  ```

The core and pacod-id run as an unprivileged user with a read-only file system. The commands
below therefore run them with your own user id and give them a directory of the host:

```sh
install -d -m 700 identity-move
idmove() { docker compose -f deploy/docker-compose.yml run --rm --no-deps \
             --user "$(id -u):$(id -g)" -v "$PWD/identity-move:/move" "$@"; }
```

**1. Export.**

```sh
idmove core identity export --out /move/gw-identity.json
```

It writes the users, admin tokens and role bindings as stored (with their password and token
hashes), the invitations, the organisations and workspaces, the sign-in settings and the
gateway's roles to a new file that only its owner may read, and prints what it holds. It
refuses to overwrite a file. Treat the file like a secret.

**2. Dry run.**

```sh
idmove id import gw /move/gw-identity.json --dry-run
```

It prints, as JSON, what would be created, changed and left as it is, what is skipped and
why, and the conflicts. With conflicts it ends with "N conflicts: nothing was imported". A
conflict is most often an address pacod-id already has under another account, such as a
first administrator created with the gateway admin's address: delete that account at
pacod-id, or give it another address, and repeat the dry run. Accounts are never merged.

**3. Import.**

```sh
idmove id import gw /move/gw-identity.json
```

All or nothing. The `pgwa_…` tokens work at pacod-id with the gateway's pepper, which compose
passes as `PID_LEGACY_PEPPERS`. Then delete the file and its directory: `rm -r identity-move`.

**4. Switch.** Set `PGW_AUTH=oidc` in `deploy/.env` and run `dc up -d --wait`, which
recreates the core, the console and the tester. With several management instances, restart
each with `PGW_AUTH=oidc`; gateways that serve only the data plane need nothing. People sign
in at pacod-id with their old password; scripts keep their `pgwa_…` tokens. The gateway's
sign-in settings travel in the export but are not applied: set the realm's fields
(above) where the gateway's values should stay, for example `remember_me_days = 30`.

**Going back** is possible while pacod-id has recorded no change to people or roles since the
import. Ask, while `deploy/.env` still says `PGW_AUTH=oidc` (the command asks pacod-id):

```sh
dc run --rm --no-deps core identity rollback
```

It answers "nothing was written at pacod-id since the import of …: set PGW_AUTH=local and
restart every core instance", or names the first changes and refuses. In the first case set
`PGW_AUTH=local` and run `dc up -d --wait`; the gateway's own copy was kept unused. After a
refusal, stay with OIDC: going back would lose those changes. A second import is refused for
the same reason.

**5. Finalize**, once the new sign-in has proved itself. It deletes the gateway's own users,
admin tokens, role bindings and invitations, with the console sessions and one-time links,
and from then on the core refuses to start with `PGW_AUTH=local`. Make a backup first; there
is no other way back.

```sh
dc run --rm --no-deps core identity finalize --yes
```

It is refused before pacod-id has the import; while an instance that serves the management
API runs with `PGW_AUTH=local`, or has not caught up with pacod-id's feed, within the last
minute (it names them: restart them with `PGW_AUTH=oidc`, and wait a minute); and while
someone added, changed or deleted users, admin tokens or role bindings here after the export,
or sent an invitation (it names them: export and import again, which brings what is new and
retires what the gateway no longer has). Until it runs, the old password hashes stay in the
gateway's database, unused.

### Deleted accounts and erasure

When pacod-id deletes an account, the core forgets it in one transaction:
its copy of the user or admin token and their role bindings (until finalize), its console
sessions and one-time links, and its name in the records of what it did, which then read
"deleted account" (the audit log's actor, the author of template versions, the inviter of
invitations). Then the core confirms the deletion to pacod-id, and tries again if that
fails.

The usage data the gateway holds about one user of a workspace is erased with
`POST /api/v1/orgs/{org}/workspaces/{ws}/erasure`, in either mode, by anyone who may administer
the workspace, a product's service account with that role included:

```sh
curl -fsS -X POST http://core:8081/api/v1/orgs/<org id>/workspaces/<workspace id>/erasure \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'content-type: application/json' \
  -d '{"end_user":"alice"}'        # or {"user_id":"..."}, as the calls carried it
# {"usage_events":4,"audit_records":0,"end_user_rollups":0}
```

It removes the usage events, the audit-mode records (with attachments no other record needs)
and, by `end_user`, the daily end-user rollups, and answers once ClickHouse has rewritten the
data. Hourly rollups carry neither and stay. The audit log records that an erasure was made,
not whose.

### Backup

`deploy/scripts/backup.sh` dumps pacod-id's database `pacod_id` next to the gateway's, as
`pacod-id.dump`, and `restore.sh` restores it, with its role (section 7). The dump holds
password hashes and sealed keys: keep it like the gateway's, and keep `PID_KEK`, `PID_KEK_ID`
and `PID_TOKEN_PEPPER` apart from it. Both were tried on the development stack: after a
restore, people sign in as before.

### Kubernetes

The Helm chart (0.2.0) carries pacod-id as the subchart `id`; the value `auth` is
`PGW_AUTH`, `ingress.id.host` is `PGW_ID_HOST`, and the Secret `pacod-id-secrets` holds
pacod-id's keys and the three client secrets. See "Signing in: pacod-id" in
`deploy/helm/pacod-gw/README.md`. With your own PostgreSQL, create pacod-id's database with
`create-id-role.sh` as above. The export, import, rollback and finalize commands need the
same environment as the services that run them (pacod-id's runbook, section 4); on a cluster
they were not exercised.

### Revocation

The identity concept's target is that the core refuses a token within two seconds of its
session ending at pacod-id. On the development stack started with `PGW_AUTH=oidc`,
`cargo run -q -p pacod-gw-testkit --bin gw-smoke -- revocation` measures it (section 11; on
2026-10-13 the slowest of 20 samples took 3 ms). It uses development-only clients and has no
counterpart for a production stack.

### Known limits

- The core connects to PostgreSQL as `pacod_gw`, which the PostgreSQL image makes a
  superuser. In the same server it could read pacod-id's database, password hashes included.
  Not changed in this release: put `pacod_id` on a server of its own, or demote the
  gateway's role to the owner of its database, which needs a change of its own
  ([threat-model.md](threat-model.md)).
- Audit log entries of invitations keep the invitee's address after their account is
  deleted.
- A rollup run at the moment of an erasure can write a day's end-user row back. Repeat the
  erasure by `end_user` after the next rollup (every five minutes).
- `invitation_link_days` and the other sign-in settings of the gateway are ignored in OIDC
  mode; the realm's apply (above).
- A member `PUT` with `role: null` for someone who is not a member answers 204 in OIDC mode,
  404 with the console's own login.

When signing in fails, see section 10, "Signing in fails with pacod-id".

## 17. The minimal gateway

`pacod-gw-lite` is a second way to run the gateway: its data plane alone, in one container,
without PostgreSQL, ClickHouse, console, management API, pacod-id or metrics. Its whole
configuration is `deploy/lite/.env`: providers and their keys, models with their prices,
pools, and one key per calling application with its limits. Every call is a line in the
container's log, with its cost.

It is a separate installation, not a role of this one: its compose project (`pacod-gw-lite`),
network and port (127.0.0.1:14080) are its own, so it runs on the same host as the stack of
this runbook without touching it, and the stack knows nothing of it. Configuration changes
need a restart; limits are counted in memory per instance.

```sh
cp deploy/lite/.env.example deploy/lite/.env   # then fill it in
make lite-check
make lite-up
```

[lite.md](lite.md) describes its configuration, logging, deployment and limits.
