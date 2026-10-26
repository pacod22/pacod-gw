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
17. [The minimal gateway](#17-the-minimal-gateway)
18. [Plugins](#18-plugins)
19. [The shared networks pacod-net and pacod-admin-net](#19-the-shared-networks-pacod-net-and-pacod-admin-net)

All commands are run from the repository root. Section 15 lists the `make` targets that
wrap the most common of them. `dc` stands for:

```sh
dc() { docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.standalone.yml "$@"; }
```

The stack is two compose files: `deploy/docker-compose.yml` holds the gateway's own services
and `deploy/docker-compose.standalone.yml` its copies of the components that other pacod
products share (PostgreSQL, pacod-id, Caddy, the monitoring). Every service is on the external
Docker network `pacod-net` (section 19). The service and volume names in the records above are
those of the time, before the rename of section 19.

## 1. What runs

pacod-gw is a gateway between applications and LLM providers. Applications send their
requests to the gateway with a gateway key. The gateway forwards them to the provider with
the provider's key, counts tokens and cost, and enforces limits.

The compose stack starts these services. The container names are the service names.

The gateway's own services (`deploy/docker-compose.yml`):

| Service | What it is | Keeps data in |
|---|---|---|
| `pacod-gw` | The core: data plane, management API, metrics. | volume `gw-spool` (events waiting to be shipped) |
| `gw-analytics` | Stores usage events, answers reports, runs batch jobs. | nothing of its own |
| `gw-console` | The admin console. | nothing |
| `gw-tester` | Runs test cases against the gateway for the console's Testing page (platform admins only). | volume `gw-tester-data` (run history, uploaded suites) |
| `gw-clickhouse` | Usage events, audit-mode records, rollups. | volume `gw-clickhouse-data` |
| `gw-valkey` | The response cache's answers, shared by the gateways (section 18). | nothing: memory only, lost on restart |
| `gw-preflight` | Runs once at every start and checks that the secrets are set. | nothing |

Its copies of the shared components (`deploy/docker-compose.standalone.yml`):

| Service | What it is | Keeps data in |
|---|---|---|
| `pacod-postgres` | Configuration, encrypted provider credentials, audit log, limit counters; pacod-id's database. | volume `pacod-postgres-data` |
| `pacod-db-init` | Runs once at every start: creates the gateway's and pacod-id's PostgreSQL roles and databases where missing, and sets pacod-id's role's password. | nothing |
| `pacod-id` | pacod-id: signs people in with `PGW_AUTH=oidc` and holds their accounts and roles (section 16). It runs in either mode. | its database `pacod_id` in `pacod-postgres` |
| `pacod-caddy` | TLS and the only entry from outside. | volumes `pacod-caddy-data`, `pacod-caddy-config` (certificates) |
| `pacod-otel` | Receives traces from the core. | nothing |
| `pacod-prometheus` | Scrapes the core's metrics, evaluates the alert rules. | volume `pacod-prometheus-data` |
| `pacod-alertmanager` | Sends the alerts by email and to a webhook (section 10). | volume `pacod-alertmanager-data` |
| `pacod-grafana` | Dashboards. | volume `pacod-grafana-data` |

With `docker-compose.load.yml` (load tests from the console, off by default):

| Service | What it is | Keeps data in |
|---|---|---|
| `gw-load-agent` | Makes the load of the console's load tests, for the tester ([testing.md](testing.md), section 9). | nothing |
| `gw-load-mock` | The mock provider behind load tests against the mock. Never a provider of real traffic. | nothing |

Requests keep flowing when PostgreSQL, ClickHouse, the analytics service or Valkey is down.
The core works from the last configuration it loaded and collects events in its spool;
without Valkey every call is a miss of the response cache.

With the scale layout, `pacod-gw` serves only the management API and the services
`gw-gateway-1`, `gw-gateway-2` serve the data plane, each with a spool volume of its own; gateways on other
machines run `deploy/node/` (section 9). Load agents on other machines run
`deploy/load-agent/` (section 9).

The minimal gateway (`deploy/lite/`, section 17) is not part of this stack: one container,
`pacod-gw` in the compose project `pacod-gw-lite`, that keeps no data and needs none of the
services above. Since it carries the core's name on `pacod-net`, it does not run on the same
host as this stack.

## 2. Ports

Only Caddy publishes ports on the host.

| Host port | Variable | Serves |
|---|---|---|
| 80 | `PGW_HTTP_PORT` | Redirect to HTTPS, and the certificate challenge. |
| 443 (TCP and UDP) | `PGW_HTTPS_PORT` | The API host, the console host and pacod-id's host (`PGW_ID_HOST`). |

On the network `pacod-net`, by service name, and on `pacod-admin-net` by alias:

| Service | Port | Serves | Reachable from outside |
|---|---|---|---|
| `pacod-gw` | 8080 | Data plane: `/v1`, `/gw/v1`, `/proxy`, `/healthz`, `/readyz` | Yes, through Caddy on the API host. |
| `pacod-gw-admin` | 8081 | Management API: `/api/v1`. On `pacod-admin-net` only. | No. The console, the analytics service and the tester call it on `pacod-admin-net`. |
| `pacod-gw` | 9100 | `/metrics` | No. |
| `gw-analytics` | 8090 | Event ingest and reports | No. |
| `gw-console` | 3000 | Admin console | Yes, through Caddy on the console host. |
| `gw-tester` | 8095 | The tester's API, for the console | No. |
| `gw-load-agent` | 8096 | A load agent's API, for the tester (with `docker-compose.load.yml`; on networks of its own, not on `pacod-net`) | No. |
| `gw-load-mock` | 9000 | The mock provider of load tests, for the gateway (with `docker-compose.load.yml`) | No. |
| `pacod-id` | 8070 | pacod-id's sign-in pages and OpenID Connect endpoints | Yes, through Caddy on `PGW_ID_HOST`. |
| `pacod-id-internal` | 8071 | pacod-id's internal API: tokens, the feed of roles, its admin API. On `pacod-admin-net` only. | No. The core, the console and the tester call it on `pacod-admin-net`. |
| `pacod-id` | 9170 | pacod-id's `/metrics`, `/healthz` and `/readyz` | No. |
| `pacod-postgres` | 5432 | PostgreSQL | No. |
| `gw-clickhouse` | 8123 | ClickHouse HTTP interface | No. |
| `gw-valkey` | 6379 | Valkey, the response cache's store | No. |
| `pacod-otel` | 4318, 4317, 13133, 8888 | OTLP over HTTP, OTLP over gRPC, health, own metrics | No. |
| `pacod-prometheus` | 9090 | Prometheus | No. |
| `pacod-alertmanager` | 9093 | Alertmanager | No. |
| `pacod-grafana` | 3000 | Grafana | Only for allow-listed addresses, at `/grafana/` on the console host. |
| `pacod-caddy` | 2019 | Caddy's admin endpoint, loopback only | No. |

Every service, the databases included, is on the network `pacod-net`, which other pacod
stacks on the same host join too. It has a route out: the databases are no longer on a network
of their own without one (section 19).

The management API and pacod-id's internal listener are on the second shared network,
`pacod-admin-net`, which is internal (no route out). The core binds 8081 to `pacod-gw-admin`
and pacod-id binds 8071 to `pacod-id-internal`: aliases each has on `pacod-admin-net` alone.
A container that listens on `0.0.0.0` is reachable on every network it joins, so the
binding, not the network, is what keeps the rest of `pacod-net` out. Their clients join
`pacod-admin-net` and call the aliases; calling `pacod-gw:8081` or `pacod-id:8071` finds
nothing. The development stack (section 15) keeps both listeners on every interface, because
it publishes them to the host on 18081 and 18071, and a published port never reaches an
internal network. `PGW_ADMIN_LISTEN` and `PID_INTERNAL_LISTEN` take an address or a host name
with a port; a name is resolved when the listener binds.

Caddy also listens on 8088 inside the stack (`http://pacod-caddy:8088`): the balanced data
plane without TLS, for the tester of the scale layout and the load agent. It is not published.

With `docker-compose.hub.yml` (gateways on other machines, section 9) the central host
publishes on its private address `PGW_CLUSTER_BIND`:

| Host port | Variable | Serves |
|---|---|---|
| 5432 | `PGW_CLUSTER_POSTGRES_PORT` | PostgreSQL, for the gateways. |
| 8090 | `PGW_CLUSTER_ANALYTICS_PORT` | The analytics service: usage events from the gateways. |
| 4318 | `PGW_CLUSTER_OTLP_PORT` | The trace collector. |
| 6380 | `PGW_CLUSTER_VALKEY_PORT` | Valkey, with TLS only: the response cache's answers, shared with the gateways. |

A gateway on another machine (`deploy/node/`) publishes on its private address
`PGW_NODE_BIND` the data plane (`PGW_NODE_DATA_PORT`, 8080) for the central Caddy and its
metrics (`PGW_NODE_METRICS_PORT`, 9100) for the central Prometheus.

A load agent on another machine (`deploy/load-agent/`) publishes its API on its private
address `LOAD_AGENT_BIND`, port `LOAD_AGENT_PORT` (8096), for the central tester.

The minimal gateway (section 17) publishes its data plane, without TLS, on
`PGW_LITE_BIND:PGW_LITE_PORT`, by default 127.0.0.1:14080. It uses none of the ports above, but
its name on `pacod-net` is `pacod-gw`, so it does not run on the same host as this stack.

## 3. Secrets

All settings are in `deploy/.env`. The file is git-ignored. `deploy/.env.example` lists
every variable with a comment.

| Variable | What it protects | Generate with | If it is lost or changed |
|---|---|---|---|
| `PGW_KEK` | Key-encryption key. Provider credentials in PostgreSQL are encrypted under it. | `openssl rand -base64 32` | **The stored provider credentials cannot be read. They must all be entered again.** |
| `PGW_KEY_PEPPER` | Gateway keys and admin tokens are stored as an HMAC under it. | `openssl rand -base64 32` | Every gateway key and admin token stops working. |
| `PGW_INTERNAL_TOKEN` | The calls between core and analytics. | `openssl rand -hex 32` | Can be changed at any time (section 8). |
| `POSTGRES_PASSWORD` | PostgreSQL's superuser `postgres`. Only `pacod-db-init` logs in with it; the core never sees it. | `openssl rand -hex 24` | Can be changed (section 8). |
| `GW_DB_PASSWORD` | The PostgreSQL user `pacod_gw`, which the core and every gateway connect as. It owns the gateway's database and is not a superuser. | `openssl rand -hex 24` | Can be changed (section 8). |
| `CLICKHOUSE_PASSWORD` | The ClickHouse user `pacod_gw`. | `openssl rand -hex 24` | Can be changed (section 8). |
| `PGW_VALKEY_PASSWORD` | Valkey, which holds the response cache's answers. | `openssl rand -hex 24` | Can be changed at any time: restart `gw-valkey` and the gateways; the kept answers are lost, which costs only misses. |
| `PGW_ADMIN_PASSWORD` | The first administrator of the console. | `openssl rand -base64 18` | Used only once; change it in the console. |
| `GRAFANA_ADMIN_PASSWORD` | The Grafana administrator. | `openssl rand -base64 18` | Used only when Grafana first starts. |
| `PGW_SMTP_PASSWORD` | The SMTP account that sends account emails (section 13). | From the mail provider | Account emails fail until it is corrected; nothing else is affected. |

pacod-id's secrets (`PID_DB_PASSWORD`, `PID_KEK`, `PID_TOKEN_PEPPER`, the three client
secrets and the console's `PGW_UI_SESSION_SECRET`) are listed in section 16. Preflight
requires them whichever `PGW_AUTH` says, because pacod-id always runs.

Rules:

- `POSTGRES_PASSWORD`, `GW_DB_PASSWORD`, `CLICKHOUSE_PASSWORD` and `PGW_VALKEY_PASSWORD` may
  contain only letters, digits and `. _ ~ -`. The PostgreSQL passwords become part of a URL.
  `openssl rand -hex` produces such values.
- `GW_DB_PASSWORD` differs from `POSTGRES_PASSWORD`. With the same value the core would hold
  the superuser's password; preflight refuses it.
- Provider API keys are not in this file. They are entered in the console and stored
  encrypted in PostgreSQL.
- The compose files pass these secrets to the containers as variables from `deploy/.env`.
  Outside compose, the core can also read them from files, where secrets are mounted as
  files: `PGW_KEK_FILE`, `PGW_KEK_OLD_FILE`, `PGW_KEY_PEPPER_FILE`,
  `PGW_INTERNAL_TOKEN_FILE`, `DATABASE_URL_FILE`, `PGW_ADMIN_PASSWORD_FILE`,
  `PGW_SMTP_PASSWORD_FILE` and `PGW_VALKEY_PASSWORD_FILE` each name one file. Or `PGW_SECRETS_DIR` names a directory with
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
printf 'GW_DB_PASSWORD=%s\n'         "$(openssl rand -hex 24)"
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
default), because this stack runs pacod-id either way.

`POSTGRES_PASSWORD` belongs to PostgreSQL's superuser `postgres`. The core connects as
`pacod_gw` with `GW_DB_PASSWORD`, a role that owns the gateway's database and nothing else:
`pacod-db-init` creates it (`platform/postgres/gw-db.sh`). The core cannot read pacod-id's
database in the same server.

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

**Step 6. Start.** Create the shared networks once (`make network` does the same, and also
stops when `pacod-admin-net` exists without being internal), then start:

```sh
docker network inspect pacod-net >/dev/null 2>&1 || docker network create pacod-net
docker network inspect pacod-admin-net >/dev/null 2>&1 || docker network create --internal pacod-admin-net
dc up -d --wait
```

`--wait` returns when every service is healthy. The first start takes a few minutes:
PostgreSQL and ClickHouse initialise their volumes, the core and the analytics service
create their schemas, Grafana creates its database, and Caddy obtains certificates.

If the command stops with `service "gw-preflight" didn't complete successfully`, a secret is
missing or malformed. `dc logs gw-preflight` names it.

**Step 7. Verify.**

```sh
dc ps                                         # every service "running (healthy)"; gw-preflight "exited (0)"
curl -s https://api.example.com/readyz        # {"status":"ready","config_version":...}
```

Open `https://console.example.com/`, log in with `PGW_ADMIN_EMAIL` and
`PGW_ADMIN_PASSWORD`, and change the password. With `PGW_AUTH=oidc` you sign in at pacod-id
instead, as section 16 describes.

With `PGW_TLS=internal`, clients do not trust the certificate. Either pass the root
certificate to them (`dc exec pacod-caddy cat /data/caddy/pki/authorities/local/root.crt`) or use
`curl -k` for a test.

**Step 8. Set up the gateway in the console.** Add a provider from its preset and give it
its API key (see "Setting up each provider" at the end of this section), activate a model,
create an organisation and a workspace, and create a gateway key. Applications then call
`https://api.example.com/v1` with that key.

**Step 9. Schedule the backup** (section 7).

**Optional: Grafana.** Grafana is served at `https://<console host>/grafana/`, and only to
the addresses in `PGW_ADMIN_ALLOW_CIDRS`. The default admits nobody. Set it to your office
or VPN range, for example `PGW_ADMIN_ALLOW_CIDRS=203.0.113.0/24`, and run
`dc up -d pacod-caddy`. Log in as `GRAFANA_ADMIN_USER`. The four dashboards are in the folder
`pacod-gw`. Check in `dc logs pacod-caddy` which client address Caddy sees before you rely on the
list: on some Docker setups (IPv6 clients without IPv6 container networking) every client
appears with a private address. Set `PGW_GRAFANA_URL` to the same address
(`ui.grafanaUrl` in the Helm chart) and the console's Monitoring page links to Grafana;
unset, the link is hidden.

**Optional: the management API from outside.** Scripts and CI may need it. Uncomment the
block marked for it in `platform/caddy/gw.caddy`, and put Caddy on `pacod-admin-net`, the only
network the management API listens on, with an override file next to the others:

```yaml
services:
  pacod-caddy:
    networks: [pacod-net, pacod-admin-net]
```

It is then served at `https://<API host>/api/v1/` for the addresses in
`PGW_ADMIN_ALLOW_CIDRS`. Without that block, call it from a container on `pacod-admin-net`;
from `pacod-net` alone it does not answer:

```sh
docker run --rm --network pacod-admin-net curlimages/curl -s http://pacod-gw-admin:8081/readyz
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
| OpenAI | `openai` | GPT-5.6 does not reason on Chat Completions in a request that offers function tools: it takes only the effort `none` there, and refuses a request without an effort, because its default is `medium`. Its capability preset `openai-gpt-5-6` (capability `reasoning_with_tools` off) makes managed mode send `none` on such requests and say so when the caller asked for another effort. A GPT-5.6 model created with `openai-reasoning` needs that preset applied, or its tool calls fail with 400. Capability presets `openai-gpt-5-6`, `openai-reasoning`, `openai-chat`. |
| Anthropic | `anthropic` | – |
| Google Gemini | `gemini` (Gemini's own API), `gemini-openai` (its OpenAI-compatible endpoint) | Use `gemini` unless something needs the compatible endpoint: it returns thought text and takes Gemini's own tools. Both can exist side by side under different names. The key is a Gemini API key from Google AI Studio. Google's SDKs reach every model through the Gemini interface (below), or Gemini itself through proxy mode: base URL `https://<API host>/proxy/<provider name>`. On the shared credential, Gemini's files, context caches and file search stores are refused; a workspace that needs them gets a credential of its own. Capability presets `gemini-3-flash`, `gemini-3-flash-lite`, `gemini-3-pro`, `gemini-2-5`, and `embedding` for `gemini-embedding-001`. |
| Gemini on Vertex AI | `vertex` (a service account), `vertex-express` (an API key) | For `vertex`, create a service account in the Google Cloud project, give it the role Vertex AI User (`roles/aiplatform.user`) on that project and nothing else, and create a JSON key for it. In the preset's base URL replace `YOUR-PROJECT` with the project id and the location (twice) with the region the models run in. Add the key file as the credential: the credential dialog takes the file, or paste its content. The gateway stores it encrypted, shows only the account and the key id, and gets access tokens with it from Google's token endpoint, which the gateways must reach (`oauth2.googleapis.com`). The token opens the whole project: proxy mode allows only model calls of that project and location, and on the shared credential refuses Cloud Storage objects, the project's data stores and model routing; IAM is what limits the key otherwise, so keep the service account to this one role and project. `vertex-express` takes an express-mode API key instead. Vertex AI has no model list: add models by hand (the ids are Gemini's, for example `gemini-3.8-flash`), with the same capability presets as Gemini. Rotate a key file like any key (section 8): add the new one, then delete the old key in Google Cloud. |
| DeepSeek | `deepseek` | Thinks by default; the effort `none` switches it off. The gateway sends earlier reasoning back on turns with tools. Off-peak rates apply all weekend and on weekdays outside 01:00–04:00 and 06:00–10:00 UTC: LiteLLM's list (key prefix `deepseek`) carries them as the variant `off_peak` with its windows, or enter them by hand (`variants.off_peak` and `windows`). They apply all day on Chinese public holidays too: the preset links the provider to the holiday calendar `cn` (section 12, Holiday calendars); a DeepSeek provider created before needs `holidays` set to `cn`. Capability presets `deepseek`, `deepseek-vision`. |
| GLM | `glm` (`https://api.z.ai/api/paas/v4`), `glm-cn` (`https://open.bigmodel.cn/api/paas/v4`) | The coding plan has its own address, `https://api.z.ai/api/coding/paas/v4`: change the base URL after choosing `glm`. `glm-cn` is priced in CNY, which needs an exchange rate (section 12, Currencies), and its prices rise with the input length: enter them as tiers. Earlier reasoning reaches the model only with preserved thinking: set the quirk `request_fields` to `{"thinking": {"clear_thinking": false}}`. A quota error (codes 1113, 1308–1311 and 1313–1321) becomes 402 `upstream_quota_exhausted`; a rate limit (1302) or an overload (1305) stays 429 and is retried. LiteLLM lists GLM under the key prefix `zai`. Capability presets `glm-5`, `glm-5-vision`, `glm-4`. |
| Kimi | `kimi` (`https://api.moonshot.ai/v1`), `kimi-cn` (`https://api.moonshot.cn/v1`) | A key works only in the region that issued it. The gateway leaves sampling parameters out (Kimi fixes them) and always sends earlier reasoning back. LiteLLM lists Kimi under the key prefix `moonshot`. Capability presets `kimi-k3`, `kimi-k2`. Checked against Kimi's documentation and the mock only. |
| xAI | `xai` (`https://api.x.ai/v1`) | Grok's reasoning tokens are reported apart from `completion_tokens`; the gateway bills them as output. The reasoning models refuse `stop` and the penalties, which their capability presets leave out. xAI marks Chat Completions as deprecated: the chat interfaces use it while it lasts, and a Responses caller is relayed to its Responses API (quirk `responses_api`, set by the preset). LiteLLM lists xAI under the key prefix `xai`. Capability presets `xai-reasoning`, `xai-mini`, `xai-chat`. Checked against xAI's documentation and the mock only. |
| Ollama | `ollama` (local), `ollama-cloud` (`https://ollama.com/v1`) | A local server needs no key. Inside the stack, `localhost` is the container: use the host's address (`http://host.docker.internal:11434/v1` on Docker Desktop) and add it to `PGW_EGRESS_ALLOW_PRIVATE`. Keep the server reachable from the gateways only. Discovery reads each model's capabilities from `/api/show` and proposes local models as free. Ollama Cloud needs a key from ollama.com, sent as Bearer; its models are not proposed as free. Capability presets `ollama`, `ollama-thinking`. Checked against Ollama's documentation and the mock only. |
| Groq | `groq` (`https://api.groq.com/openai/v1`) | Open models through an OpenAI-compatible API, and Whisper (`whisper-large-v3`, `whisper-large-v3-turbo`) priced per second of audio (`per_audio_second`): managed mode asks Groq for `verbose_json` to learn each file's duration, and writes the caller's `json` or `text` from it. Checked against Groq's documentation and the mock only. |
| Cohere | `cohere` (`https://api.cohere.com`) | Rerank models only (`rerank-v3.5`), at `v2/rerank`, priced per search unit (`per_search_unit`: one query with up to 100 documents). A Cohere provider takes models of kind `rerank` only; Cohere's chat models are reached through its OpenAI-compatible endpoint as an `openai` provider. Discovery lists the models that serve rerank. Checked against Cohere's documentation and the mock only. |
| Jina AI | `jina` (`https://api.jina.ai/v1`) | Rerank and embeddings, priced by tokens. A self-hosted vLLM or Infinity server serves Jina's rerank API: set it up as an `openai` provider at its own address (the quirk `rerank_api` is `jina` by default). Checked against Jina's documentation and the mock only. |
| Voyage AI | `voyage` (`https://api.voyageai.com/v1`) | Rerank and embeddings, priced by tokens. Its rerank API differs (`top_k`, `data`): the preset sets the quirk `rerank_api: voyage`. Checked against Voyage's documentation and the mock only. |
| TypeSafe (Jev) | `typesafe` (`https://api.typesafe.ai/v1`) | Decision models for the intent plugin's Jev engine (section 18), and for applications at `POST /v1/systemone` or through proxy mode (`/proxy/<provider>/v1/systemone`). Granting the model to a workspace is what lets its applications send text to TypeSafe: the plugins' approval of third parties governs plugins only. Chat refuses decision models, and model lists leave them out. Add the model by its versioned id (`jev-1.13.0`) with kind `decision` and the capability preset `decision`; the aliases `jev-latest` and `jev-preview` move when TypeSafe releases. Price: input tokens only ($0.042 per million when this was written; output 0). Grant it to the organisations whose bindings use it. Activation probes `GET /v1/models` with the key. Text sent to it leaves the installation: the plugin's install must approve the third party `typesafe` (section 18). Checked against TypeSafe's documentation and the mock only. |

`make smoke-live` checks a live installation's providers with the keys in `.env`
(`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `ZAI_API_KEY`,
`TYPESAFE_API_KEY` for one Jev classification through the intent plugin,
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
| `gw-preflight` | exit status | All required secrets are set and well-formed. | `dc logs gw-preflight` names the missing or malformed value. |
| `pacod-postgres` | `pg_isready` over TCP | The server accepts connections. | Still initialising, wrong volume ownership, disk full. |
| `gw-clickhouse` | `GET /ping` | The server answers. | Still initialising, out of memory, disk full. |
| `pacod-gw` | `GET :8080/readyz` | A configuration snapshot is loaded and the spool accepts events. | PostgreSQL unreachable at start, a missing secret, the spool's disk takes no writes (read-only, permissions), or the spool is full while the installation-wide `on_spool_full` is `fail_closed` (section 10, "The spool grows"). |
| `gw-analytics` | `GET :8090/readyz` | The ClickHouse schema is in place. | ClickHouse unreachable or wrong password. The service keeps retrying. |
| `gw-console` | `GET :3000/` | The console answers. | The process crashed; see `dc logs gw-console`. |
| `pacod-db-init` | exit status | The gateway's role and database exist and the role has `GW_DB_PASSWORD`; pacod-id's role and database exist and the role has `PID_DB_PASSWORD`. | PostgreSQL refused `POSTGRES_PASSWORD`, or the installation predates the superuser `postgres` (section 6); see `dc logs pacod-db-init`. |
| `pacod-id` | `GET :9170/readyz` | pacod-id has migrated its database and loaded its signing key. | A missing or wrong `PID_*` setting, or a mail setting it refuses (section 10, "Signing in fails with pacod-id"); see `dc logs pacod-id`. The core waits for it. |
| `pacod-caddy` | its admin endpoint | Caddy runs with a loaded configuration. | An error in the Caddyfile or in a variable it uses. |
| `pacod-prometheus` | `GET /-/ready` | Prometheus has loaded its data. | A broken rule file, or the volume is full. |
| `pacod-grafana` | `GET /api/health` | Grafana and its database are up. | First start still running (up to five minutes are allowed). |

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

`pacod-otel` has no health check in compose because its image contains no tool to run one.
Its own endpoint is `http://pacod-otel:13133/` on `pacod-net`.

## 6. Upgrade

1. Read the release notes of the new version.
2. Make a backup (section 7).
3. Get the new images: build them from the new release with its tag
   (`make images VERSION=<version>`). No images are published for you; if you push them to a
   registry of your own, set `PGW_REGISTRY=<registry>/` in `deploy/.env` to pull them from
   there.
4. Set `PGW_VERSION` in `deploy/.env` to the new tag.
5. Apply:

   ```sh
   git pull            # the compose files, Caddyfiles, dashboards and rules belong to the release
   dc pull --ignore-pull-failures   # images built on this host cannot be pulled; that is fine
   dc up -d --wait
   ```

Compose replaces only the containers whose image or settings changed. Files mounted into a
container, such as the Caddyfile and the Prometheus rules, are read again only on a restart:
after a release that changes `deploy/caddy`, `deploy/prometheus` or `platform/`, run
`dc restart pacod-caddy pacod-prometheus`. The core migrates
the PostgreSQL schema when it starts, and the analytics service migrates ClickHouse. No
separate migration step is needed. To apply the PostgreSQL migrations without starting the
gateway, run `dc run --rm pacod-gw migrate`.

**Order: analytics before the core.** When a release adds fields to the usage event, the
analytics service must know them before the core sends them. An older analytics service
accepts the newer events but drops the fields it does not know, and they cannot be recovered
later. With Compose, `dc up -d --wait gw-analytics` first, then `dc up -d --wait`; with Helm,
roll out the analytics Deployment first. The analytics service answers `503` until its own
ClickHouse migrations have run, and the core keeps the events in its spool until then.
The ClickHouse migrations `0003_attribution` (the caller's `agent_id`, `user_id`, `task` and
`complexity`), `0010_plugins` (the intent, the plugins' verdict and time, `parent_id`) and
`0011_response_cache` (`cache`, `cache_saved_cost`), `0012_cache_saved_reporting`
(`cache_saved_cost_reporting`) and `0013_usage_units` (audio and image tokens, `images`,
`audio_ms`, `search_units`) are such changes.

**Upgrading to the release with OpenAI files and batches.** The PostgreSQL migration
`0017_provider_objects_api` adds the column `api` to the record of files and batches; it runs
when the core starts. Set the quirk `openai_objects` on OpenAI providers made before this
release to serve OpenAI's `/v1/files` and `/v1/batches` through them. A call on the
OpenAI-compatible wire that names a `file_id` the gateway has no record of is now refused on
the shared credential, as it was for Anthropic: upload such files through the gateway, or
give the workspace a credential of its own. While instances of the older release run, they
answer `/v1/files` without `anthropic-version` with `404`, and their upkeep polls OpenAI
batches at Anthropic's path, which fails and is retried until a newer instance settles them.

**Upgrading to the release where applications call System One.** A decision model granted
to an organisation (for the intent plugin's Jev engine) is now also callable by its
applications at `/v1/systemone` and through proxy mode, by every key without a model list.
The organisation setting `plugin_third_parties` governs plugins only. Before upgrading,
review the grants of decision models, and give `allowed_models` to the keys of applications
that must not send text to TypeSafe.

**Upgrading to the release with the Responses API.** `POST /v1/responses` is served by
every chat model, stateless. Its calls have the interface `responses` in the request log,
in routing rules and in limit filters: a rule or filter that names `openai` does not match
them. A provider is spoken to in the Responses API only when its quirk
`responses_api` is set, which the `openai` and `xai` presets now do. Providers made before
this release do not have it: set it on them (console, Providers, the provider's quirks, or
`"quirks": {"responses_api": true}` through the API), or their Responses calls are translated
to chat completions, where the Responses API's own tools (web search, file search, code
interpreter) are refused. Proxy mode on the installation's shared credential now refuses
`previous_response_id`, `conversation`, stored prompts and item references on
`/v1/responses` (`403 own_credential_required`): what the provider keeps for that credential
is every workspace's.

**Upgrading to the release with image, transcription, rerank and realtime models.** Roll out
the analytics service first (`0013_usage_units`). The release adds the model kinds `image`,
`transcription`, `rerank` and `realtime`, the provider family `cohere`, and the rates
`input_audio`, `output_audio`, `input_image`, `output_image`, `per_image`, `per_audio_second`
and `per_search_unit`. No PostgreSQL migration is needed: configuration objects are JSON
documents. While instances of the older release still run, add no model of a new kind and no
Cohere provider: an older instance skips what it cannot read (and logs it as a problem of the
snapshot), so it answers `404` for those models, and it ignores the new rates, so it records
calls that use them as unpriced.

**Upgrading to the release with the response cache.** It adds the service `gw-valkey` and
the secret `PGW_VALKEY_PASSWORD`: generate it (`openssl rand -hex 24`) into `deploy/.env`
before step 5, or preflight stops the start. The core installs the response cache plugin on
its first start, with a default binding for every workspace (section 18) in the mode
`PGW_CACHE_DEFAULT_MODE` gives, `off` unless set: switch it on in the console when you want
calls with temperature 0 answered from the cache, best to `shadow` first to see what it would
answer. Roll out the analytics service first, as above.

**Upgrading to the release that clears the response cache.** Nothing to do beyond the order
above (`0012_cache_saved_reporting`). Until every instance runs it, an instance of the older
release ignores clears and keeps answering from what it holds in its own memory; with Valkey
the cleared keys are removed for it too.

**Upgrading to the release with the response cache's scope.** A binding's settings gain
`scope` (section 18), and bindings stored without it are kept per API key: answers one key's
calls were given no longer reach another key of the workspace. Set `scope` to `workspace` to
share them as before. Every kept answer is a miss once after the upgrade, since the key of an
answer now names its scope. An instance of the older release does not compile a binding with
a setting it does not know, and the console writes `scope` into every response cache binding
it saves: an older instance then has no cache for the workspaces of that binding until it is
upgraded, and serves their calls from their providers. Upgrade the instances before changing
response cache bindings, or accept those misses meanwhile.

**Upgrading to the release that installs the response cache off.** An installation that has
the response cache installed keeps it as it is: nothing changes on an upgrade. Only a core
whose database never had the install installs it, and then off unless `PGW_CACHE_DEFAULT_MODE`
says `shadow` or `enforce`. The minimal gateway's cache is off unless `PGW_CACHE_MODE` says
otherwise: set `PGW_CACHE_MODE=enforce` in `deploy/lite/.env` to keep it on.

**Upgrading to the release that seals the response cache's answers.** Answers go into Valkey
sealed and under other names, so every answer kept before is a miss once; the old ones expire
in their time. An instance of the older release and one of this release do not find each
other's answers while both run. With `docker-compose.hub.yml`, the central host now publishes
Valkey's TLS port only (6380), and needs a certificate before step 5 (section 9, "Valkey
between the machines"). Until each gateway on another machine has `rediss://`, the port and
the authority's certificate in its `.env`, it runs without the cache; its calls are served as
before.

**Upgrading to the release where the core is not PostgreSQL's superuser.** Before it, the
core connected as `pacod_gw`, the superuser the PostgreSQL image created, and could read and
change pacod-id's database in the same server. A new installation now has the superuser
`postgres`, and `pacod_gw` owns the gateway's database and nothing else. An existing
installation is changed once, in step 5, after `git pull` and `dc pull` and in place of
`dc up -d --wait`:

1. In `deploy/.env`, add `GW_DB_PASSWORD` with the value `POSTGRES_PASSWORD` has now, and
   give `POSTGRES_PASSWORD` a new value (`openssl rand -hex 24`). The core and the gateways on
   other machines keep the password they have; the superuser gets one they never held.
2. Stop the gateways on other machines (`docker compose -f deploy/node/docker-compose.yml stop`
   on each).
3. Run `deploy/scripts/demote-db-role.sh`. With the scale or hub layout, set `COMPOSE_FILE`
   first, as for a restore (section 7). The script stops the gateways of this host, renames
   the superuser `pacod_gw` to `postgres` with the new `POSTGRES_PASSWORD`, makes `pacod_gw`
   anew as the owner of the database and of everything in it, and starts the stack. The
   renaming and the handing over are one transaction: all or nothing.
4. Start the gateways on other machines again. Their `DATABASE_URL` keeps working.

The script refuses, with nothing changed in PostgreSQL, while `POSTGRES_PASSWORD` is still the
old value, while `GW_DB_PASSWORD` is missing or equal to it, and while anything else is
connected as the superuser; it names such sessions. Run again, it finds nothing to do. The
data plane is down from the script's start until the core is ready again: 23 seconds on the
development stack.
Without the change, preflight stops the start (`GW_DB_PASSWORD is not set`), and
`pacod-db-init` fails (`password authentication failed for user "postgres"`). A backup of the
older release restores into the newer one.

**Rolling back past it** does not work on the changed volume: the older release's
`pacod-db-init` logs in as `pacod_gw`, which is no longer the superuser, and fails. Start the
older release on a new PostgreSQL volume and restore the backup from step 2 there.

**Upgrading to the release with `pacod-admin-net`.** The management API and pacod-id's
internal listener move to the external network `pacod-admin-net` (section 2). Compose does not
start without it. `make up` creates it; with plain `docker compose`, run before step 5:

```sh
docker network create --internal pacod-admin-net
```

Scripts and tools that called `http://pacod-gw:8081` or `http://pacod-id:8071` from a
container now call `http://pacod-gw-admin:8081` and `http://pacod-id-internal:8071`, from a
container on `pacod-admin-net`. The old addresses find nothing. The images of the release
before refuse the aliases as listen addresses, so pacod-id and the core start only with the
new images.

**Upgrading to the release on the shared network.** The services, their containers and their
volumes have new names, and the stack runs on the external network `pacod-net`. Section 19
says what to do once: create the network and move the data into the volumes of the new names
before step 5.

**Upgrading to the release with pacod-id.** This release adds the services `id` and `id-db`
(today `pacod-id` and `pacod-db-init`; section 16), and they run whatever `PGW_AUTH` says.
Before step 5:

- Add pacod-id's secrets to `deploy/.env`, generated as in section 4, step 3:
  `PID_DB_PASSWORD`, `PID_KEK`, `PID_TOKEN_PEPPER`, `PGW_ID_CLIENT_SECRET`,
  `PGW_UI_CLIENT_SECRET`, `TESTER_ID_CLIENT_SECRET` and `PGW_UI_SESSION_SECRET`. They are
  required even with `PGW_AUTH=local`: without them preflight stops the start and
  `dc logs gw-preflight` names each one. Store `PID_KEK` and `PID_TOKEN_PEPPER` apart from the
  backups (section 3).
- Set `PID_KEK_ID` before `pacod-id` first starts (for example `2026-10`); preflight does not check
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

`pacod-db-init` creates pacod-id's role and database in the existing PostgreSQL at the first
start; with a PostgreSQL the compose file does not run, see section 16. The core now starts only
once `pacod-id` is healthy, in either mode, so a pacod-id that cannot start also keeps the core
from starting (section 10, "Signing in fails with pacod-id"). The release changes
`deploy/caddy` and `deploy/prometheus`: run `dc restart pacod-caddy pacod-prometheus` after step 5.
Afterwards `dc ps` shows `pacod-id` healthy and `pacod-db-init` exited (0).

During the replacement of the core the data plane is down for the time the new container
needs to become ready. Running streams get up to 50 seconds to finish. For an upgrade
without that gap, run several gateway instances and replace them one after the other
(section 9, "Upgrading several instances").

**Rolling back.** Set `PGW_VERSION` back and run `dc up -d --wait`. This works as long as
the newer version made no schema change the older one cannot read; the release notes say
so. Otherwise restore the backup from step 2.

**Upgrading the third-party images** (PostgreSQL, ClickHouse, Caddy, Prometheus, Grafana,
the collector): their tags are pinned in `deploy/docker-compose.yml` and
`deploy/docker-compose.standalone.yml` and change with a
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

1. Do steps 1 to 3 of the first deployment, and create the network `pacod-net` (step 6), but
   put the **original** `PGW_KEK`,
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
`COMPOSE_FILE=deploy/docker-compose.yml:deploy/docker-compose.standalone.yml:deploy/docker-compose.scale.yml`; the script then
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

A key the gateway already stores can be given to an organisation or a workspace without
entering it again. On the settings page, Provider keys, "Use a stored key" lists the keys that
may be given there; on the platform's Credentials page of a provider, "Give to…" on a key lists
the organisations and workspaces it may go to. The secret never leaves the gateway.

| Key | Can be given to | By |
|---|---|---|
| The installation's | any organisation or workspace | platform admins |
| An organisation's | that organisation's own workspaces | its admins and platform admins |
| A workspace's | nowhere | – |

The organisation or workspace gets a copy: a credential of its own with the same fingerprint,
made current unless asked otherwise. Rotating or disabling the key it came from does not change
the copy, so after rotating a key that was given, give the new key again: made current, it
retires the old copy like any rotation. Given the installation's shared key, an organisation or workspace
uses it as its own: its proxy calls may then reach files, batches and the blind relay on the
provider account that every workspace shares, which the shared key otherwise refuses. Disabled
keys and keys the place already has are not offered. The audit log records each copy as `copy`,
with the key it came from in `detail.from`.

The same through the management API (see section 4 for how to reach it):

```sh
curl -X POST http://pacod-gw-admin:8081/api/v1/providers/<provider id>/credentials \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "content-type: application/json" \
  -d '{"label":"2026-10 rotation","secret":"<new provider key>","make_current":true}'

# Give a stored key to an organisation (or {"type":"workspace","id":"<workspace id>"}).
curl -X POST http://pacod-gw-admin:8081/api/v1/providers/<provider id>/credentials/<credential id>/copy \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "content-type: application/json" \
  -d '{"scope":{"type":"organisation","id":"<organisation id>"}}'
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

3. Restart the core: `dc up -d --wait pacod-gw`, and every other instance with the same three
   values: the local gateways of the scale layout and gateways on other machines (section
   9). From now on every credential that is added or rotated is encrypted under the new
   key. Existing ones are still read with the old key.

4. Once **every** instance runs with the new key, wrap the existing credentials under it.
   An instance still on the old key could not read them afterwards, so both ways below
   refuse while a running instance has not confirmed the new key, and name it. Each credential's data key is
   decrypted with the old key and encrypted with the new one; the secrets themselves are not
   touched. Either run the command once:

   ```sh
   dc run --rm pacod-gw rewrap-secrets
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

The response cache seals the answers it keeps in Valkey with keys derived from the current
key only (section 18). After step 3 an instance no longer finds the answers kept under the old
key: they are misses once and expire in their time. There is nothing to rewrap.

### Other secrets

| Secret | How |
|---|---|
| `PGW_INTERNAL_TOKEN` | Change it in `deploy/.env`, then `dc up -d --wait pacod-gw gw-analytics`. Events wait in the spool while the two restart. |
| `POSTGRES_PASSWORD` | `dc exec pacod-postgres psql -U postgres -c '\password postgres'` (it asks for the new one twice and puts it on no command line), then change `deploy/.env`. The variable alone does not change the password of an existing volume. Nothing but `pacod-db-init` uses it. |
| `GW_DB_PASSWORD` | Change it in `deploy/.env`, then `dc up -d --wait`: `pacod-db-init` sets it on the role `pacod_gw` and the core restarts with it. Gateways on other machines need the new value in their `DATABASE_URL`; until then they cannot connect. |
| `CLICKHOUSE_PASSWORD` | Change it in `deploy/.env`, then `dc up -d --wait gw-clickhouse gw-analytics`. The ClickHouse image writes the user from the variable whenever its container is created. |
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
- **The response cache's answers**, when the gateways reach one Valkey (`PGW_VALKEY_URL`):
  an answer one gateway keeps is answered by every other. Without it each gateway keeps its
  own, and a repeated call hits only on the gateway that saw it first.
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
| One core | `docker-compose.yml` + `docker-compose.standalone.yml` | The default: one core with the role `all`. |
| Several gateways on one host | `+ docker-compose.scale.yml` | `pacod-gw` becomes the management instance, `gw-gateway-1` and `gw-gateway-2` serve the data plane. |
| Gateways on other machines | `+ docker-compose.hub.yml` on the central host, `deploy/node/` on each other machine | One central console and management instance; any number of gateways. |
| Kubernetes | `deploy/helm/pacod-gw` | A gateway StatefulSet and an admin Deployment. |
| Minimal gateway | `deploy/lite/docker-compose.yml` | A separate installation without database or console (section 17). Several instances share nothing: each counts its own limits. |

In every layout Caddy on the central host balances the data plane over the gateways named
in `PGW_DATA_UPSTREAMS`: each request goes to the gateway with the fewest calls in flight,
only to gateways that answer `/readyz`, and a request whose gateway cannot be reached is
sent to another one.

### Several gateways on one host

```sh
dcs() { docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.standalone.yml \
          -f deploy/docker-compose.scale.yml "$@"; }
dcs up -d --wait          # or: make scale-up
```

`pacod-gw` keeps its name and its alias `pacod-gw-admin` on `pacod-admin-net`, so the
console, the analytics service, the tester and the backup scripts need no change; it serves
only the management API now. The gateways join `pacod-net` only: they would inherit the alias
otherwise. The tester calls the data plane through Caddy, as applications do.

To add a gateway, copy the `gw-gateway-2` block in `deploy/docker-compose.scale.yml`, give it
the next number, its container name and a volume of its own, add it to
`deploy/prometheus/targets-scale/gateways.yml`, and set
`PGW_DATA_UPSTREAMS="gw-gateway-1:8080 gw-gateway-2:8080 gw-gateway-3:8080"` in `deploy/.env`.
Then `dcs up -d --wait`.

Check: the console's System page lists `core` (role admin; the instance id, not the service
name) and the gateways `gateway-1` and `gateway-2`, all on the
newest configuration version. `dcs config --volumes` lists one spool volume per gateway.

### Gateways on other machines

```
                      clients
                         │ HTTPS
┌────────────────────────▼─────────── central host ──────────────────────────────────┐
│  Caddy ──── console ──── pacod-gw (admin)        PostgreSQL   analytics   OTLP     │
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
| gateways | central host | `PGW_CLUSTER_VALKEY_PORT` (6380) | the response cache's Valkey, with TLS |
| central host | gateways | `PGW_NODE_DATA_PORT` (8080) | Caddy, the data plane |
| central host | gateways | `PGW_NODE_METRICS_PORT` (9100) | Prometheus |
| gateways | providers | 443 | the LLM providers |

**On the central host:**

1. In `deploy/.env`, set `PGW_CLUSTER_BIND` to the host's private address, and the ports
   if 5432, 8090, 4318 or 6380 are taken there. Put Valkey's certificate and key for that
   address into `deploy/valkey/tls/` ("Valkey between the machines" below).
2. Start the stack with the hub file, alone or with the scale file:

   ```sh
   dch() { docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.standalone.yml \
             -f deploy/docker-compose.scale.yml -f deploy/docker-compose.hub.yml "$@"; }
   dch up -d --wait
   ```

**On each other machine** (Docker and a checkout of the repository at the same release):

3. Get the core image of the same `PGW_VERSION`: build it (`docker build -f
   deploy/docker/rust.Dockerfile --target core -t pacod-gw-core:<version> .`) or pull it from
   your registry (`PGW_REGISTRY`).
4. `cp deploy/node/.env.example deploy/node/.env` and fill it in: a unique
   `PGW_INSTANCE_ID`; the machine's private address as `PGW_NODE_BIND`; the central host's
   private address in `DATABASE_URL`, `PGW_ANALYTICS_URL`, `PGW_OTLP_ENDPOINT`,
   `PGW_VALKEY_URL` and `PGW_TRUSTED_PROXIES`; and the shared secrets, copied from the central
   `deploy/.env`. `chmod 600 deploy/node/.env`. Copy the authority of Valkey's certificate to
   `deploy/node/tls/valkey-ca.pem` first: the gateway does not start while the file
   `PGW_VALKEY_CA_FILE` names is missing. To run it without the shared cache, empty
   `PGW_VALKEY_URL` instead; the CA file is then not read.
5. Start it:

   ```sh
   docker compose -f deploy/node/docker-compose.yml up -d --wait
   ```

   The preflight step names anything missing. Within a few seconds the console's System page
   lists the gateway with the newest configuration version. An alert about different
   secrets means step 4 copied a wrong value.

**Back on the central host:**

6. Add the gateway to `PGW_DATA_UPSTREAMS` in `deploy/.env`, listing every gateway, local
   ones included: `PGW_DATA_UPSTREAMS="gw-gateway-1:8080 gw-gateway-2:8080 10.0.0.11:8080"`. Then
   `dch up -d pacod-caddy`.
7. Copy `deploy/prometheus/targets/nodes.yml.example` to `nodes.yml` beside it and list the
   gateway's metrics address with its instance id. Prometheus reads the file within 30
   seconds.
8. Check: the console's Monitoring page lists the gateway, and its figures move when calls
   arrive.

Every provider must be reachable from every machine. A provider on a private address must be
reachable from each gateway and named in each gateway's `PGW_EGRESS_ALLOW_PRIVATE`.

**Removing a gateway:** take it out of `PGW_DATA_UPSTREAMS` and run `dch up -d pacod-caddy`
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

**Valkey between the machines** goes over TLS. `docker-compose.hub.yml` publishes only
Valkey's TLS port (6380, `PGW_CLUSTER_VALKEY_PORT`); the gateways on the central host stay on
the plain port inside `pacod-net`. Before the first start with the overlay, put a certificate
for the central host's private address and its key into `deploy/valkey/tls/` (or the directory
`PGW_VALKEY_TLS_DIR` names): `deploy/valkey/tls/README.md` makes both with an authority of your
own. Without the files `gw-valkey` serves no TLS and the gateways on other machines run
without the cache. On each gateway on another machine, put the authority's certificate into
`deploy/node/tls/valkey-ca.pem` and set, in its `.env`:

```sh
PGW_VALKEY_URL=rediss://10.0.0.10:6380/0
PGW_VALKEY_CA_FILE=/run/pacod-gw-tls/valkey-ca.pem
```

A certificate from an authority the image already trusts needs no `PGW_VALKEY_CA_FILE`. The
answers reach Valkey sealed in any case (section 18): TLS keeps its password and the names of
the workspaces off the network. A gateway that reaches a Valkey on another host with plain
`redis://` logs a warning at start.

**TLS between the machines** is otherwise optional on a private network. For PostgreSQL: mount a
certificate and key into the `pacod-postgres` service with a further override file (the key owned
by the `postgres` user, mode 0600), add `-c ssl=on -c ssl_cert_file=... -c
ssl_key_file=...` to its command, and append `?sslmode=require` (or `verify-full` with
`sslrootcert=`) to the gateways' `DATABASE_URL`. This has not been exercised.

### Connections to PostgreSQL

PostgreSQL accepts 100 connections (`max_connections` in `docker-compose.standalone.yml`). A core with
the role `admin` or `all` keeps up to 10, a gateway up to 4 (`PGW_DB_MAX_CONNECTIONS`
changes either). A backup or a `psql` session needs a few more. With one admin instance
that leaves room for about 20 gateways; for more, raise `max_connections` and the memory of
the `pacod-postgres` service.

### Upgrading several instances

Upgrade one instance after the other; the data plane stays up.

1. The analytics service first (section 6).
2. The management instance: `dcs up -d --wait pacod-gw`. It applies the PostgreSQL migrations.
3. Each gateway, one at a time: `dcs up -d --wait gw-gateway-1`, then `gw-gateway-2`, then on each
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
   scale or hub files), with `TESTER_LOAD_TOKEN` in deploy/.env and `gw-load-mock` in
   `PGW_EGRESS_ALLOW_PRIVATE`. In the scale layout set `LOAD_AGENT_TARGET_URL=http://pacod-caddy:8088`.
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

On the central host, name it in `TESTER_LOAD_AGENTS` (`local=http://gw-load-agent:8096,eu-1=http://<private address>:8096`)
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
dc logs gw-preflight              # are the secrets complete?
```

The gateway's own services and pacod-id write one JSON object per line
(`PGW_LOG_FORMAT=json`), without request bodies or keys; Caddy writes its own JSON. Docker keeps
5 files of 10 MB per service. "Logs" below has the details.

Where to look:

| Question | Place |
|---|---|
| Is the gateway up and what version of the configuration runs? | `curl https://<API host>/readyz` |
| What is failing right now? | Console: Monitoring. Prometheus alerts: `http://pacod-prometheus:9090/alerts` on `pacod-net`, or Grafana. The gateway-delay chart draws the targets of the global setting `delay_targets` (proxy and managed mode) as dashed lines. |
| What happened to one request? | Console: Requests. Search by the `x-gw-request-id` response header. |
| Who changed the configuration? | Console: System, audit log. |
| Trends | Console: Metrics (platform roles; needs `PROMETHEUS_URL` for the console, set in the compose file, `ui.prometheusUrl` in the Helm chart). Grafana: Overview; Providers and models; Workspaces; Gateway internals, the same panels. |
| Traces | The collector logs a summary (`dc logs pacod-otel`). For full traces, configure a backend in `deploy/otel-collector/config.yaml`. Each request has a span, and each provider attempt a child span with the provider's request id. |

### Logs

The gateway's services log after the convention all pacod products share,
`pacod-demo/docs/LOGGING.md` in the pacod-demo repository: one event per line on standard
output (gw-mock: standard error), in JSON or text, with the same keys everywhere. Docker keeps
5 files of 10 MB per service, for every service of every compose file
(`x-logging` in each file).

| Variable | Read by | Values |
|---|---|---|
| `RUST_LOG` | core, analytics, load agent, gw-mock | `debug`, `info`, `warn`, `error` (any case; `warning` is `warn`), or a filter: `info,pacod_gw_core=debug`. Anything else logs at `info` and says so once. Default `info`. |
| `PGW_LOG_FORMAT` | core, gw-mock; in Compose also the analytics service (`ANALYTICS_LOG_FORMAT`), the tester (`TESTER_LOG_FORMAT`), the load agent (`LOAD_AGENT_LOG_FORMAT`) and pacod-id (`PID_LOG_FORMAT`) | `json` or `text`, any case. Anything else is `json`. The production compose files default to `json`, the development ones to `text`. |
| `TESTER_LOG_LEVEL` | tester | `debug`, `info`, `warn`, `error`. Default `info`. |
| `PGW_CADDY_LOG_FORMAT` | Caddy's access and runtime log | `json` (production) or `console` (development; Caddy's name for text). |
| `SERVICE_NAME` | every pacod process | The `service` key. The compose files set the service name (`pacod-gw`, `gw-analytics`, `gw-gateway-1`, ...); unset, each binary uses its own. |

A JSON line starts with five keys, always in this order, and the event's fields follow at the top
level with their JSON types:

```json
{"timestamp":"2026-10-23T06:41:07.695Z","level":"warn","service":"pacod-gw","logger":"pacod_gw_core::snapshot::reload","message":"configuration poll failed; serving the last snapshot","error":"pool timed out while waiting for an open connection"}
```

| Key | Content |
|---|---|
| `timestamp` | UTC, RFC 3339 with milliseconds. |
| `level` | `trace`, `debug`, `info`, `warn` or `error`. |
| `service` | `SERVICE_NAME`. |
| `logger` | Where the event comes from: the Rust module (`pacod_gw_core::snapshot::reload`), the tester's Python logger (`pgw_tester`, `uvicorn.error`). |
| `message` | What happened. |

A field that would take one of these five names gets a trailing underscore (`message_`). An
exception in the tester adds `error` and `stack_trace`. The text format holds the same in one
line, with values that contain spaces quoted:

```text
2026-10-23T06:41:07.695Z WARN  pacod-gw pacod_gw_core::snapshot::reload: configuration poll failed; serving the last snapshot error="pool timed out while waiting for an open connection"
```

Caddy keeps its own keys (`ts`, `logger`, `msg`, `request`, ...) and leaves out the query
parameters `token`, `code`, `state`, `next`, `flow`, `id_token_hint`, `login_hint` and `key`,
the headers `Referer`, `X-Api-Key` and `X-Goog-Api-Key`, and the `Location` of redirects
(section 13, "Links in the logs"); it leaves out `Cookie`, `Set-Cookie` and `Authorization` by
itself.

The databases, Prometheus, Grafana and the collector keep the formats of their images.

To read one service, or to find a request, in the production stack (JSON; `fromjson?` skips any
line that is not):

```sh
dc logs -f --no-log-prefix pacod-gw | jq -cR 'fromjson? | select(.level == "warn" or .level == "error")'
dc logs --no-log-prefix pacod-caddy | jq -cR 'fromjson? | select((.request.uri // "") | startswith("/v1/"))'
```

On Kubernetes the chart sets `RUST_LOG` from `core.logLevel` and `analytics.logLevel`, and the
formats from `logFormat`.

### Alert delivery

Prometheus evaluates the alert rules and hands what fires to the stack's Alertmanager, which
sends it by email and to a webhook when `deploy/.env` says where:

- `PGW_ALERT_EMAIL_TO`: one or more addresses, separated by commas. The mail goes out with
  the SMTP account of the account emails (`PGW_SMTP_*`, section 13); without
  `PGW_SMTP_HOST` nothing is sent by email.
- `PGW_ALERT_WEBHOOK_URL`: an HTTP endpoint that receives Alertmanager's JSON.

Both empty: the alerts are shown in Prometheus, Grafana and the console only. The
configuration is written from these values at every start of the `pacod-alertmanager` service
(`deploy/alertmanager/render.sh`); change them and run `dc up -d pacod-alertmanager`. The
development stack publishes Alertmanager on port 19093. With the Helm chart, set
`metrics.alertmanagerConfig` (an AlertmanagerConfig for the Prometheus Operator's
Alertmanager).

The console's Monitoring page lists the same conditions as open alerts, computed by the
core from the last minutes of every instance: over 15 minutes, error rate above 10 % with at
least 20 calls and more than five timeouts; over 10 minutes, more than one rejection a
second for one reason and the gateway delay's 99th percentile above its target; a nearly full audit spool, besides limits, prices,
providers, spools, configuration and instances.

### The stack does not start

- `service "gw-preflight" didn't complete successfully`: `dc logs gw-preflight` names each secret
  that is missing, too short or malformed. Fix `deploy/.env` and start again.
- `pacod-gw` restarts in a loop: `dc logs pacod-gw`. The message names the variable
  (`PGW_KEK is not set`, `DATABASE_URL is not set`, `PGW_INTERNAL_TOKEN must be set`).
  `password authentication failed for user "pacod_gw"` means the role does not have
  `GW_DB_PASSWORD`: `pacod-db-init` sets it at every start, so see its log.
- `service "pacod-db-init" didn't complete successfully`: `dc logs pacod-db-init`.
  `password authentication failed for user "postgres"` means `POSTGRES_PASSWORD` was changed
  in `deploy/.env` but not in PostgreSQL (section 8), or the installation was made while
  `pacod_gw` was the superuser and has not been changed yet (section 6, "Upgrading to the
  release where the core is not PostgreSQL's superuser").
- `pacod-caddy` is unhealthy: `dc logs pacod-caddy`. With Let's Encrypt, the two DNS names must point
  to this host and ports 80 and 443 must be reachable from the internet.
- A port is in use: set `PGW_HTTP_PORT` and `PGW_HTTPS_PORT`. Let's Encrypt needs the
  standard ports.
- Compose reports the `pacod-id` container as unhealthy, or never starts the core: the core waits
  for pacod-id in either mode. See "Signing in fails with pacod-id" below.

### Logging in to the console fails

This is the console's own login (`PGW_AUTH=local`). With `PGW_AUTH=oidc`, see the next
subsection.

- "Cross-site POST form submissions are forbidden": the console builds its own address
  from the `Host` header and `X-Forwarded-Proto`. Something between the browser and the
  console changes `Host` or does not set the protocol. Open the console only through Caddy,
  under `PGW_CONSOLE_HOST`. If another proxy or load balancer sits in front of Caddy, list
  it under `trusted_proxies` in the Caddyfile and raise `XFF_DEPTH` of the `gw-console` service by
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

- **`pacod-id` does not become healthy** (`dc logs pacod-id`). It names the variable: a missing `PID_*`
  secret or client secret, `PID_EMAIL_FROM_ADDRESS is needed with PID_SMTP_HOST` (set
  `PGW_EMAIL_FROM_ADDRESS`), `PID_SMTP_USERNAME is set without PID_SMTP_PASSWORD`, or a
  signing key it cannot decrypt because `PID_KEK` or `PID_KEK_ID` changed. `pacod-db-init` must
  have exited with 0 first (`dc logs pacod-db-init`).
- **503 `identity_unavailable`** from the management API ("sign-in is not available right now
  (pacod-id); try again shortly"): the core's copy of pacod-id's feed is older than
  `PGW_ID_MAX_STALENESS` (60 seconds), or pacod-id does not answer on its internal listener.
  `dc ps pacod-id`, then `dc logs --tail 50 pacod-gw`. Only the management API is affected; the data
  plane never asks pacod-id and keeps serving. It recovers by itself once pacod-id answers.
- **pacod-id's sign-in works, but the console asks for the password again and again**: the
  core refuses the console's token, so the console forgets it. The core, the console and the
  tester must run with the same `PGW_AUTH`: after changing it, run `dc up -d --wait`, which
  recreates all three, not `dc up -d gw-console` alone. Outside compose, the core's `PGW_ID_ISSUER`
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
   stack. `dc exec pacod-caddy nslookup api.openai.com` must resolve, and
   `dc exec pacod-caddy wget -S --spider https://api.openai.com/ 2>&1 | head -3` must show an
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
  `anthropic_betas_allowed` setting (Platform settings, Calls) does not list the
  `anthropic-beta` value the caller sent. Unset allows every beta feature.
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

A call is also settled without a cost when it used something its price has no rate for. Each
kind of model bills by its own units:

| Kind | Rates |
|---|---|
| `chat`, `embedding`, `decision` | Tokens: `input`, `output`, the cache rates, `reasoning` |
| `speech` | `per_character` |
| `image` | `per_image`, or tokens with `input_image` and `output_image` (gpt-image, Gemini). A size or quality priced differently is a variant named `<quality>-<size>`, `<quality>` or `<size>`, for example `hd-1024x1792` |
| `transcription` | `per_audio_second` (Whisper), or tokens with `input_audio` (gpt-4o-transcribe) |
| `rerank` | `per_search_unit` (Cohere), tokens (Jina, Voyage), or `per_request` |
| `realtime` | Tokens with `input_audio` and `output_audio` |

Audio and image tokens without their own rate are charged at `input` and `output`.

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

1. `dc ps gw-analytics gw-clickhouse`: are both healthy?
2. `dc logs --tail 50 gw-analytics`: ClickHouse errors, for example a full disk or a wrong
   password.
3. `dc logs --tail 50 pacod-gw | grep -i ingest`: can the core reach the analytics service,
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

Do not delete the `gw-spool` volume to "fix" a backlog. It holds the events that have not been
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
3. `dc ps pacod-postgres`. If PostgreSQL is down, traffic continues on the last snapshot and the
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

- *The tester does not answer*: check `docker compose ps gw-tester` and its log. The console
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
- *An agent is not reachable*: check `docker compose ps gw-load-agent` and its log, or on another
  machine `make load-agent-up` there and the firewall in between. *Refused the tester's token*
  means the agent's `LOAD_AGENT_TOKEN` differs from the tester's `TESTER_LOAD_TOKEN`.
- *The call before the load answered 401 or 403*: the key is wrong or revoked, or does not
  allow the model or the mode. No load was made.
- *Mock target, the call before the load fails*: the core refuses the mock's private address.
  Add `gw-load-mock` to `PGW_EGRESS_ALLOW_PRIVATE`; preflight warns about it.
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

### A plugin blocks calls or fails

A call refused by a plugin answers `403` with the plugin's error type (`intent_blocked` for
the intent plugin) in the caller's dialect, and the message of the rule that blocked it. Its
usage event has `plugin_verdict: block`, and the intent and its confidence. To see why:

- Open the call in **Insight → Requests**: the intent, the engine and the verdict are there.
  When the call was flagged or audit mode was on, its audit record has the whole verdict
  under `resolution.plugins`: the rule that decided, the probabilities, the rationale.
- Switch the binding to `shadow` while you look: calls are served, and the events still say
  what the binding would have done (`would_block`).
- A rule in trial (`"shadow": true` in the catalogue) is recorded and never acts.

A binding that cannot answer refuses calls while it enforces with `on_error: closed`:

| Error type | Means | Do |
|---|---|---|
| `503 plugin_unavailable`, retryable | The engine timed out, its provider failed, or its answer did not parse. `plugin_error` on the event says which. | Check the engine's model like any model (section 10, "A provider or model fails"); raise `timeout_ms`; or set `on_error: open` to let calls through meanwhile. |
| `429`/`402` of a limit | The workspace's own limit refused the engine's call; the call would hit it too. | As for any limit. |
| `503 plugin_misconfigured` | No credential for the engine's provider, the model is gone, or this instance lacks the plugin. Retrying does not help. | `GET /api/v1/system/status` lists the binding under `problems`; fix the binding or the model. |

The time the plugins take before the provider call is `plugin_ms` on the event, apart from
the gateway delay; the metric is `gw_plugin_duration_seconds`, on the Gateway internals
dashboard with the verdicts, errors and cache hits.

Alerts: `GatewayPluginErrors` when more than 5 % of a plugin's answers are errors for 10
minutes; `GatewayPluginFlagged` when a rule with `"alert": true` flagged calls. The flagged
calls are in the audit records (**Insight → Audit records**), unless the organisation forbids
audit mode.

### The response cache fails

A store that fails never fails a call: the call is a miss and its provider answers. The
alert `GatewayResponseCacheErrors` fires when an instance's store failed 20 times in 10
minutes for 5 minutes; the Gateway internals dashboard shows the errors (`get`, `put`, and
`skipped`: after three failures in a row the store is left alone for five seconds), the hit
rate and the time of a lookup.

- `dc ps gw-valkey` and `dc logs gw-valkey`: is it running and healthy? Its health check
  asks it with the password from `deploy/.env`.
- `PGW_VALKEY_PASSWORD` must be the same for `gw-valkey` and every gateway; a gateway on
  another machine also needs `PGW_VALKEY_URL` pointing at the central host with `rediss://`,
  the port `PGW_CLUSTER_VALKEY_PORT` open, and the authority of Valkey's certificate
  (section 9).
- `NOAUTH` or `WRONGPASS` in the gateway's log at debug level: the passwords differ.
  `invalid peer certificate: UnknownIssuer`: `PGW_VALKEY_CA_FILE` does not name the authority
  of `gw-valkey`'s certificate. `NotValidForName`: the certificate does not carry the address
  in `PGW_VALKEY_URL` as an IP subject alternative name.
- Every instance must have the same `PGW_KEK`: one with another key neither finds nor opens
  the answers the others keep, and its own are misses for them. The console's critical alert
  for a differing key-encryption key says so too.
- Restarting `gw-valkey` empties the cache; nothing else is lost.
- **Platform › Response cache** shows whether this instance reaches Valkey, with its error
  when it does not, and each instance's store breaker: `open` while that instance skips the
  store, with the failures in a row before it opened.

Answers that are not what a caller expects:

- `x-gw-cache: hit` and `Age` on the response say it came from the cache. The call's audit
  record (when one is kept) names the call it was kept from under `resolution.cache`.
- The caller sends `x-gw-cache: refresh` for a fresh answer, which replaces the kept one, or
  `off` to leave the cache out.
- To switch it off: the workspace's or organisation's **Plugins** page → a binding of
  `response_cache` with mode `off`; or the platform's install (**Plugin installs**) disabled.

### PostgreSQL, ClickHouse or the disk

- PostgreSQL down: requests are served; limits are counted in memory and written when it
  returns, each instance enforcing only its own count meanwhile; the console cannot save
  changes, and an instance that restarts cannot start.
- ClickHouse or analytics down: requests are served; events wait in the spool; reports in
  the console answer 503.
- Valkey down: requests are served by their providers, without the response cache.
- Disk full: check `docker system df -v` for the volumes. ClickHouse and Prometheus are
  the ones that grow. Prometheus is capped at 8 GB and `PROMETHEUS_RETENTION`.

## 11. Development stack

For work on the code, the override files build the images from the working tree, add a
mock provider and publish the services on the host: `docker-compose.dev.yml` for the
gateway's own services, `docker-compose.standalone.dev.yml` for the shared components. It needs no `deploy/.env`: every
secret has a fixed, public development value. The production secrets in `deploy/.env` do not
change them, since the development volumes keep the passwords and keys they were made with
(compose reads `deploy/.env` for both stacks); other settings, such as provider API keys,
still come from it.

```sh
docker network inspect pacod-net >/dev/null 2>&1 || docker network create pacod-net
docker network inspect pacod-admin-net >/dev/null 2>&1 || docker network create --internal pacod-admin-net
docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.dev.yml \
  -f deploy/docker-compose.standalone.yml -f deploy/docker-compose.standalone.dev.yml \
  -f deploy/docker-compose.load.yml -f deploy/docker-compose.load.dev.yml up -d --build --wait
```

`make dev-up` and `make dev-down` do the same. The development stack always has a load agent
and the load tests' mock (`docker-compose.load.dev.yml`).

A development volume made while `pacod_gw` was PostgreSQL's superuser stops `pacod-db-init`.
Change it once with the files above, or remove the volume `pacod-gw_pacod-postgres-data`:

```sh
COMPOSE_FILE=deploy/docker-compose.yml:deploy/docker-compose.dev.yml:deploy/docker-compose.standalone.yml:deploy/docker-compose.standalone.dev.yml:deploy/docker-compose.load.yml:deploy/docker-compose.load.dev.yml \
  deploy/scripts/demote-db-role.sh
```

| Service | Address on the host |
|---|---|
| Data plane | `http://localhost:18080` |
| Management API | `http://localhost:18081` |
| Core metrics | `http://localhost:19100/metrics` |
| Analytics | `http://localhost:18090` |
| Console | `http://localhost:13000` (through Caddy, so that the login works over HTTP) |
| PostgreSQL | `localhost:15433`, user `pacod_gw`, password `pacod_gw_dev_postgres`; the superuser `postgres`, password `pacod_dev_postgres_superuser` |
| ClickHouse | `http://localhost:18123`, user `pacod_gw`, password `pacod_gw_dev_clickhouse` |
| Valkey (the response cache) | `localhost:16379`, password `dev-only-valkey-password` |
| Grafana | `http://localhost:13001`, user `admin`, password `pacod-gw-dev` |
| Prometheus | `http://localhost:19090` |
| Mock provider | `http://localhost:19000`; inside the stack `http://gw-mock:9000` |
| Tester | `http://localhost:18095` (needs a platform admin's session token) |
| Load agent `local` | `http://localhost:18096` (token `dev-only-load-agent-token-0123456789abcdef`) |
| Load agent `remote`, with `make load-agent-dev-up` | `http://localhost:18196`; it reaches the stack at `host.docker.internal:18080` |
| Mock provider of load tests | `http://localhost:19001`; inside the stack `http://gw-load-mock:9000` |
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
target. On 2026-10-13: 20 samples, p50 2 ms, p95 3 ms, max 3 ms. It
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
`docker exec pacod-caddy cat /data/caddy/pki/authorities/local/root.crt`,
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
- On your machine, trust Caddy's root certificate, from the `docker exec pacod-caddy cat ...`
  command above, run on the development machine. On macOS:
  `sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain root.crt`.
  The certificate authority lives in the volume `pacod-caddy-data` and stays the same until
  the volumes are deleted.
- `PGW_DEV_BIND=0.0.0.0` instead publishes every development port on the network:
  the data plane on 18080, PostgreSQL, ClickHouse, Grafana, the mock and the tester. They
  then use the public development secrets of the table above unless `deploy/.env` on that
  machine sets its own (names as in `deploy/.env.example`) before the first start. Only do
  this on a network you trust. Grafana then needs
  `PGW_DEV_GRAFANA_URL=http://devbox.lan:13001/`.

**Several instances.** `make dev-scale-up` starts the development stack in the scale layout
(`docker-compose.scale.yml` and `docker-compose.scale.dev.yml`): the management API alone in
`pacod-gw`, and two gateways, `gw-gateway-1` and `gw-gateway-2`. The data plane on `http://localhost:18080` is then Caddy, balancing
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
   `PGW_DATA_UPSTREAMS="gw-gateway-1:8080 gw-gateway-2:8080 host.docker.internal:18180" make dev-scale-up`.
3. For Prometheus, write `deploy/prometheus/targets/nodes.yml` with
   `host.docker.internal:19180` and the instance `node-1`.

`make node-dev-down` stops it.

**The minimal gateway's development stack** (`make lite-dev-up`, section 17) does not run at
the same time as this one: both have a `pacod-gw` and a `gw-mock` on `pacod-net`. Stop one
with its `down` target before starting the other.


## 12. Audit mode, the price job, moving a configuration

Three features that need a decision or a routine from the person who runs the stack.

### Audit mode

Audit mode stores complete requests and answers: what the application sent, what the
gateway sent to the provider on each attempt, what the provider answered, and what the
application received, with the time stamps of the call. It is meant for debugging and for
workspaces that must keep such a record. It is off unless somebody switches it on.

| Where | What can be set |
|---|---|
| Workspace (console: the workspace, Settings) | Mode: off, on, or "on request" (only calls that send the header `x-gw-audit: true` or `gateway.audit` in a managed-mode body). An end date, after which it switches itself off. A sample share. Retention in days (14 by default). Whether attachments are stored in full or as size and checksum. Whether a call may proceed without its record (best effort, the default) or is refused when the record cannot be written (required). Whether Realtime sessions are recorded too (off by default). |
| Template | A switch: as the workspace says, always on, or always off for calls through this template. The other settings come from the workspace. |
| Organisation | A rule for all its workspaces: forbid audit mode, or force it. Forbid also switches the response cache off (section 18). |
| Platform (console: Platform settings, Audit) | The same rule for everything, and the largest body that is recorded (8 MiB by default; a longer body is cut and the record says so). |

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
- Realtime sessions (`/v1/realtime`) are recorded only where the option for them is set as
  well. Each response is one record: the session's events since the previous response, one
  per line, with the time each passed. The four parts are what the caller sent, what the
  gateway passed on, what the provider sent and what the caller received. Audio is kept like
  any attachment: in full, or as size and checksum, whatever the length of the frame. A
  recorded session holds up to four bodies of the largest recorded size (8 MiB each by
  default) in memory. A WebSocket session relayed in proxy mode is never recorded.

### The price job

The price job looks up the providers' prices on a schedule and keeps the model repository
current. It runs inside the analytics service, which asks the core for work every 30
seconds; only one instance runs it at a time. It is off until it is configured (console:
Platform, Pricing, or `PUT /api/v1/price-job`). While the job has no sources, the console's
form suggests a source for each provider at OpenAI's, Anthropic's, Gemini's
(`generativelanguage.googleapis.com`), DeepSeek's or z.ai's address: LiteLLM's list, with the
key prefix that provider needs. Providers elsewhere (a mock, GLM's mainland endpoint, Vertex AI)
get none. Nothing is stored until the form is saved.

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
(Platform settings, Routing, `discovery_interval_hours`; 0 runs it only when asked) and whenever
someone presses Run now (Platform, Models, Discovery; `POST /api/v1/discovery/runs`). The page
shows the last run, the schedule and the providers the last run could not ask, and updates
itself when a run finishes (with several management instances, only when the console's own
instance ran it: reload otherwise). A run that breaks off is recorded as failed with the
reason; one whose instance stops during it is closed as failed by the next upkeep round, so
later runs go on. A model the provider lists and the repository lacks becomes a proposal,
listed under its provider with what the provider reports (context window, output limit, image
input, reasoning) where it reports it. Each proposal shows the kind (chat, embedding, speech)
and the capability preset the gateway suggests, from the model's id and these reports.

Add as draft opens a dialog that creates a draft model, which still needs a price and an
activation. The dialog offers three names: `<provider>/<id>`, the provider's id alone
(`naming: bare`, as some applications name Gemini models; the choice stays for the next
model), or another name. Change the kind and the preset there where the suggestion is wrong;
check the capabilities on the draft either way. A preset left as suggested takes the
provider's report on image input; one you choose keeps its own. Dismiss keeps a proposal from
being proposed again; Dismiss all does so for every model of a provider shown, after asking.
The provider's own page (Platform, Providers, Discovery on its row) still lists a dismissed
model and can create a draft from it.

A repository model the provider no longer lists is flagged, with a link to the model; nothing
is retired by itself. Platform, Providers, Test asks one provider for its list at once, which
tests its address and its shared credential. Gemini's speech models (`-tts`) are proposed as
speech models, which managed mode does not serve on Gemini: use proxy mode for them. Vertex AI
has no model list: discovery skips its providers.

### Holiday calendars

Prices with off-peak hours apply all day on public holidays where the provider says so
(DeepSeek, on Chinese public holidays). The dates are in the platform settings (Platform
settings, Costs, Holiday calendars; `holiday_calendars` in `PUT /api/v1/settings`): a calendar
has a name, a time zone, a source and its dates. A provider names its calendar in `holidays`. The
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
`/v1/messages`: same base URL, a gateway key in `x-api-key`. A call to `/v1/files` with
`anthropic-version` (the SDK always sends it) is Anthropic's; without it, OpenAI's (see
"OpenAI files and batches" below).

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

### WebSocket sessions

Proxy mode relays a provider's WebSocket endpoint (OpenAI's Realtime API at
`/proxy/<provider>/v1/realtime?model=<id>`, Gemini's Live API where its path is allowed)
byte for byte, with the provider's credential. Browsers send the gateway key as the
subprotocol `openai-insecure-api-key.<key>`.

- **Refused** (`403`): for a key whose usage policy is `require` (a relayed session reports no
  usage: set the key to `estimate` or `ignore`, or use the managed `/v1/realtime`), in a
  workspace that enforces a plugin or restricts provider tools, on Vertex AI, and on the
  shared credential without `?model=`. Making an ephemeral Realtime key
  (`/v1/realtime/sessions`, `/transcription_sessions`, `/client_secrets`) on the shared
  credential is refused too.
- **Limits.** A session counts as one request and holds one of the key's concurrent calls
  while it lasts. It ends after five minutes without traffic and after an hour.
- **Recorded** as one request per session, with its bytes, and the reason it ended in
  `error_type` (`session_idle`, `session_too_long`, `instance_stopping`; empty when an end
  closed it).
- **A stopping instance** closes its sessions and waits for them, up to `drain_seconds`, before
  its spool stops; clients reconnect to another instance.
- **In front of the gateway**, Caddy passes upgrades on. Its `stream_close_delay` (one hour in
  `platform/caddy/gw.caddy`) keeps sessions open across a reload of its configuration. Another
  proxy or an ingress must pass `Upgrade` too, and allow idle connections of five minutes.

### Realtime sessions (managed)

`GET /v1/realtime?model=<model or @template>` opens a WebSocket session with a model of kind
`realtime` at an OpenAI-shaped provider. OpenAI's SDKs and browsers connect as they would to
OpenAI, with the gateway's address; a browser sends the gateway key as the subprotocol
`openai-insecure-api-key.<key>`.

- **Recorded** as one event per response (interface `realtime`), each with the response's
  tokens (`audio_input_tokens` and `audio_output_tokens` among them), its cost at the price in
  effect when the session opened, the provider's response id as `upstream_request_id`, and
  the session's `session_id` (the caller's `x-gw-session`, else the handshake's request id). A
  session without a response is one event with no usage. A response the caller left is
  cancelled and recorded `cancelled`, with what the provider reported within five seconds.
- **Limits.** The session counts as one request and holds one of the key's concurrent calls.
  Each response's tokens and spend count in the limits' current windows. While a token or
  spend limit has nothing left, `response.create` is answered with an `error` event
  (`quota_exceeded`, or `rate_limited` for a per-minute limit), and after a response that
  used up a limit the gateway closes the session with code 1008: with voice activity
  detection the provider starts responses on its own.
- **Refused before the session opens:** a model of another kind (`400`), a workspace that
  enforces a plugin (`403`), no budget left (`402`), a session that must be recorded while
  its records cannot be stored (`503 audit_unavailable`). Within the session, provider tools
  the workspace does not allow and a change of model are refused with an `error` event.
- **Recorded in audit mode** where audit mode applies to the session and the workspace
  records Realtime sessions (section 12): one record per response, and one more event without
  usage for events after the last response. `x-gw-audit` on the handshake asks for it in
  mode "on request"; the sample is drawn once per session. With the guarantee "required",
  `response.create` is answered with an `error` event (`audit_unavailable`) while records
  cannot be stored, and a response whose record could not be stored closes the session with
  code 1013.
- **Not covered:** plugins and the response cache. Input audio transcription is not
  accounted.
- **Times:** five minutes without an event either way (pings do not count), an hour at most; a stopping
  instance closes its sessions with code 1001.

### OpenAI files and batches

OpenAI's SDK reaches its Files and Batches APIs through the gateway with its base URL set to
the gateway's `/v1` and a gateway key: `/v1/files` (without `anthropic-version`) and
`/v1/batches`. Ownership, credentials and accounting are as for Anthropic's, above, with
these differences.

- **Which provider.** Files go to the workspace's provider whose quirk `openai_objects` is set
  (the `openai` preset sets it); with several, the caller names one with `x-gw-provider`. A
  batch goes to the provider and credential its input file was uploaded with.
- **A batch's requests are read when its file is uploaded.** A file of purpose `batch` (at most
  200 MB) is read line by line: each request is resolved, merged and filtered like a single
  call at `/v1/chat/completions`, `/v1/responses` (stateless) or `/v1/embeddings`, must be
  served by the file's provider in OpenAI's own API, must not stream, and is written into the
  file with the provider's model id. A line that cannot be served refuses the upload with
  `400`, naming the line. A batch can be made only from such a file (`400` otherwise), for the
  endpoint its lines call. The gateway does this at upload because OpenAI does not let the
  content of every file purpose be read back later. A file of another purpose is relayed as
  it came; one over 200 MB needs its `Content-Length`.
- **Accounting.** The limits are checked per request when the batch is made, from what was
  decided at upload. When the batch has ended (`completed`, `failed`, `expired` or
  `cancelled`), the upkeep task reads its output and error files and writes one usage event
  per request, as for Anthropic's (error type `batch_error` for a request in the error file).
- **Output files.** The output and error files the provider makes are recorded as the batch's
  workspace's, so the caller reads them at `/v1/files/{id}/content` through the gateway.
- **Not served:** `/v1/uploads` (large uploads in parts), deleting a batch, and listing files
  in ascending order; use proxy mode on a credential of the workspace's own for those. A
  list filtered by `purpose` may hold fewer than `limit` files while more follow.
- **File ids in calls.** On the OpenAI-compatible wire, as on Anthropic's, a call that names a
  `file_id` of another workspace is refused with `404`, and on the shared credential so is
  one the gateway has no record of. A vector store (`vector_store_ids`) or a container is
  used only on a credential of the workspace's or organisation's own (`403
  own_credential_required`).

### Currencies

Costs are computed in the currency of each model's price. When a call settles, its cost is
also converted into the reporting currency of the global settings with the exchange rate
held there (`exchange_rates`, one `EUR: 1.08` per line in the console, Platform settings,
Costs: what one unit is worth in the reporting currency). Reports, the overview, the console's
totals and spend
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
has to be reachable: section 4, "the management API from outside", or call
`http://pacod-gw-admin:8081` from a container on `pacod-admin-net`.

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
| Sent by | the core | pacod-id (the `pacod-id` service) |
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
dc run --rm --no-deps pacod-gw email-check
# and with a test message:
dc run --rm --no-deps pacod-gw email-check --send-to you@example.com
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

The links carry their token in the URL. On every host, Caddy removes from its logs the `token`
and `next` parameters (after signing in, `next` can lead back to an invitation link), the
sign-in's `code` and `state`, `id_token_hint`, `login_hint` and `flow`, gateway keys in `key`,
the `Referer` header, and the `Location` header of redirects, which carries the same values
(section 10, "Logs"). A proxy in front of Caddy should do the same.

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
| `make network` | Creates the shared network `pacod-net` once; every `up` target runs it first. | Section 19 |
| `make up`, `make down` | Starts or stops the stack with one core: the app file and the standalone file. | Sections 1 and 4 |
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
| `make dev-up`, `make dev-down` | The development stack (section 11), the app and standalone files with their development overrides and the load files: images built from the working tree, the mock providers, a load agent, ports between 10000 and 19999, no `deploy/.env` needed. `DEV_HTTPS=1` adds ports 80 and 443 with `https://console.localhost` and `https://api.localhost`. `PGW_AUTH=oidc make dev-up` signs people in through pacod-id. |
| `make dev-scale-up`, `make dev-scale-down` | The same in the scale layout: Caddy on 18080, the gateways on 18082 and 18083. `DEV_HTTPS=1` as above. |
| `make node-dev-up`, `make node-dev-down` | A gateway that plays another machine, next to the development stack (section 11). |
| `make load-agent-dev-up`, `make load-agent-dev-down` | A load agent that plays another machine (`remote`, 18196), next to the development stack. |
| `make lite-dev-up`, `make lite-dev-down` | The minimal gateway built from the working tree, with the mock provider: 18280, mock 19280 (section 17). Not while the development stack runs: both have `pacod-gw` and `gw-mock`. |
| `make run-lite` | The minimal gateway on the host, configured by `LITE_ENV` (default `deploy/lite/.env`). |
| `make test-services` | The PostgreSQL (25433), ClickHouse (28123) and Valkey (26379) containers the tests use. |
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
| `make verify` | Every check before a commit: formatting, lints, every test of the Rust workspace, the console and the tester, and the OpenAPI drift check. |
| `make deploy-check` | Static checks of `deploy/`, `platform/` and `.github/`: compose files, Caddyfiles with the gateway's Caddy fragment, Prometheus configuration with its scrape fragment, and rules, the collector, shell scripts, the Helm chart, workflows. Starts no service. |
| `make lint`, `make fmt` | Lints, or formats, the Rust code, the console and the tester. |
| `make openapi` | Writes the OpenAPI document and the console's generated client after a change to the management API. `make openapi-check` fails when they are out of date. |
| `make helm-rules` | Copies the alert rules into the Helm chart after a change to them. |
| `make helm-deps` | Packages the vendored pacod-id chart into the gateway's chart (`charts/`) after `vendor/pacod-id` changed. |
| `make tester-test` | The tester's lint, format check and tests. |
| `make deny` | Licences, banned crates, sources and advisories of the Rust dependencies, by `deny.toml`. Needs `cargo install cargo-deny --locked`. |

### Against a running stack

| Target | What it does |
|---|---|
| `make smoke-mock` | The whole flow against the mock provider: providers, models, both modes, both dialects, limits, routing, the response cache, audit mode, the price tracker, Anthropic files and batches, key revocation. |
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

pacod-id is a separate OpenID Connect provider. It is vendored in
`vendor/pacod-id` (the commit is in `vendor/pacod-id/VERSION`), released with the gateway
under the same version, and has its own runbook, `vendor/pacod-id/docs/runbook.md`. The
API is described in its [API document](../vendor/pacod-id/docs/api.md). With `PGW_AUTH=oidc` people sign in to
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
both modes, because the gateway's own stack always runs pacod-id. A stack that runs pacod-id
for several products (pacod-demo) gives the gateway's services only the client secrets and the
console's session secret, and preflight then requires them only with `PGW_AUTH=oidc`.

| Variable | Read by | What it is | Generate with |
|---|---|---|---|
| `PGW_AUTH` | core, console, tester | `local` or `oidc`. All three must agree; compose gives them the same value. | – |
| `PGW_ID_HOST` | Caddy, pacod-id; the core and the console as the issuer | pacod-id's DNS name. The issuer is `https://<PGW_ID_HOST>`. | – |
| `PID_DB_PASSWORD` | `pacod-db-init`, pacod-id | The PostgreSQL role `pacod_id`. Letters, digits and `. _ ~ -` only. | `openssl rand -hex 24` |
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
`PGW_ID_INTERNAL_URL=http://pacod-id-internal:8071` for the core, the console and the tester, and the
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
effect then. Check a changed file with `dc run --rm --no-deps pacod-id check-config`, then
`dc restart pacod-id`.

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
2. `dc up -d --wait`. `dc logs pacod-id` says "created the bootstrap admin".
3. Open `https://<console host>/`. It leads to pacod-id: sign in with the address and the
   password, and change the password at `https://<PGW_ID_HOST>/account/security`.
4. Empty both `PID_BOOTSTRAP_ADMIN_*` in `deploy/.env` and run `dc up -d --wait pacod-id`.
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

- The release with pacod-id runs with `PGW_AUTH=local`, and `dc ps` shows `pacod-id` healthy
  (section 6).
- `PID_BOOTSTRAP_ADMIN_EMAIL` and `PID_BOOTSTRAP_ADMIN_PASSWORD` are empty, and were empty at
  every start of `pacod-id`. An account pacod-id already has under one of the gateway's addresses
  is a conflict, and the import is refused.
- Make a backup (section 7).
- Agree a quiet period: from the export until the switch, nobody changes users, roles, admin
  tokens or invitations. The last step refuses what changed after the export.
- With a PostgreSQL that the compose file does not run, create pacod-id's role and database
  first, as a superuser and with psql 15 or newer (compose's `pacod-db-init` does this at
  every start). The passwords go in the environment, not on the command line:

  ```sh
  PGHOST=HOST PGUSER=postgres PGPASSWORD=... PID_DB_PASSWORD=... sh deploy/postgres/id-db.sh
  ```

The core and pacod-id run as an unprivileged user with a read-only file system. The commands
below therefore run them with your own user id and give them a directory of the host:

```sh
install -d -m 700 identity-move
idmove() { docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.standalone.yml \
             run --rm --no-deps --user "$(id -u):$(id -g)" -v "$PWD/identity-move:/move" "$@"; }
```

**1. Export.**

```sh
idmove pacod-gw identity export --out /move/gw-identity.json
```

It writes the users, admin tokens and role bindings as stored (with their password and token
hashes), the invitations, the organisations and workspaces, the sign-in settings and the
gateway's roles to a new file that only its owner may read, and prints what it holds. It
refuses to overwrite a file. Treat the file like a secret.

**2. Dry run.**

```sh
idmove pacod-id import gw /move/gw-identity.json --dry-run
```

It prints, as JSON, what would be created, changed and left as it is, what is skipped and
why, and the conflicts. With conflicts it ends with "N conflicts: nothing was imported". A
conflict is most often an address pacod-id already has under another account, such as a
first administrator created with the gateway admin's address: delete that account at
pacod-id, or give it another address, and repeat the dry run. Accounts are never merged.

**3. Import.**

```sh
idmove pacod-id import gw /move/gw-identity.json
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
dc run --rm --no-deps pacod-gw identity rollback
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
dc run --rm --no-deps pacod-gw identity finalize --yes
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
curl -fsS -X POST http://pacod-gw-admin:8081/api/v1/orgs/<org id>/workspaces/<workspace id>/erasure \
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
`deploy/postgres/id-db.sh` as above. The export, import, rollback and finalize commands need the
same environment as the services that run them (pacod-id's runbook, section 4); on a cluster
they were not exercised.

### Revocation

The target is that the core refuses a token within two seconds of its
session ending at pacod-id. On the development stack started with `PGW_AUTH=oidc`,
`cargo run -q -p pacod-gw-testkit --bin gw-smoke -- revocation` measures it (section 11; on
2026-10-13 the slowest of 20 samples took 3 ms). It uses development-only clients and has no
counterpart for a production stack.

### Known limits

- An installation made while the core's role `pacod_gw` was PostgreSQL's superuser could read
  pacod-id's database in the same server, password hashes included, until it is changed once
  (section 6, "Upgrading to the release where the core is not PostgreSQL's superuser").
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

It is a separate installation, not a role of this one: its compose project (`pacod-gw-lite`)
and port (127.0.0.1:14080) are its own, and the stack knows nothing of it. It joins
`pacod-net` under the name `pacod-gw`, the address other pacod products call the gateway at,
so it does not run on the same host as the stack of this runbook. Configuration changes
need a restart; limits are counted in memory per instance.

```sh
cp deploy/lite/.env.example deploy/lite/.env   # then fill it in
make lite-check
make lite-up
```

[lite.md](lite.md) describes its configuration, logging, deployment and limits.

## 18. Plugins

Plugins are built into the gateway and do nothing until they are switched on
([plugin concept](plugin-concept.md)). `GET /api/v1/plugins` lists those of this build:

- `intent` classifies each call against a catalogue of intents, and its rules block, flag,
  route and limit calls by the intent. It is switched on as described below.
- `response_cache` answers a call that was answered before with the kept answer. It is
  installed by default, and off in a new installation until it is switched on ("The
  response cache", below).

**Switching one on**, in the console or the API:

1. Platform: **Plugin installs** → install the plugin. Engines that send text to a third
   party (Jev sends it to TypeSafe) can be used only once the install approves that party by
   name (`third_parties`).
2. Organisation or workspace: **Plugins → Intent catalogues** → add a catalogue: the intents,
   further questions (flags, scores, choices) and the rules. The form starts from a built-in
   template, `starter` (`GET /api/v1/intent-catalogue-template`): a general assistant's
   intents, one question of each kind, and rules that flag and block. Change it, and give it a
   name of its own; a catalogue's name cannot change later. **Start empty** clears the form.
3. **Plugins** → add a binding: the plugin, the mode, and its settings, which name the
   catalogue and the engine. An organisation's binding applies to all its workspaces; a
   workspace's own replaces it, unless the organisation locked it. The form starts from a
   template the core makes for the scope (`GET …/plugin-binding-template`): shadow mode, the
   catalogue `starter` if the scope has it (else its first), and the cheapest active model
   granted there that offers JSON-schema output, or Jev when there is none. The form lists what
   keeps it from being created as it is, from the checks a creation runs, and at a workspace
   names the organisation's binding it would replace.

| Setting | Means |
|---|---|
| `mode` | `shadow` (the default) looks at calls alongside the provider call and records what it would do; no call changes. `enforce` looks before the call is planned; its verdict acts. `off` at a workspace switches the organisation's binding off there. |
| `on_error` | With `enforce`: `closed` (the default) refuses a call the plugin could not look at; `open` lets it through. |
| `timeout_ms` | How long a call waits; the default depends on the engine (LLM 3 s, Jev 1 s, cascade 4 s). |
| `expose` | Tell the caller the intent in `x-gw-intent`. Off by default: labels describe the catalogue. |
| `settings.engine` | `llm`: a chat model with JSON-schema output (`capabilities.structured_output`), named in `settings.llm.model` as a model id or `@template`. `jev`: TypeSafe's Jev, a decision model named in `settings.jev.model` (default `typesafe/jev-1.13.0`); fast, with calibrated probabilities, and it sends the text to TypeSafe. `cascade`: Jev first, and the LLM decides the intent when Jev's confidence is below `settings.cascade.escalate_below` (0.6) or it answers `unknown`. |
| `settings.scope` | What is classified: the user's newest turn (the default), the newest `turns`, or the conversation; `max_input_tokens` caps it. |
| `settings.act_min_confidence` | Routing and limits act on the intent from this confidence (0.5); below it they see `unknown`. |
| `settings.cache_seconds` | How long an answer is reused for the same text in the same workspace (600; 0 asks every time). |

**Start in shadow mode.** Watch the intents and `plugin_verdict` in the usage reports for a
while, then switch to `enforce`. A new block rule can go into trial first: `"shadow": true`
on the rule records it as `would_block`.

**What it costs.** Each classified call makes one engine call of its own (two when the
cascade asks the LLM), in the same workspace: its usage event has `task: plugin.intent`, key
`plugin:intent` and `parent_id`, the classified call's request id. A Jev call's event has
`interface: decision` and is priced by its input tokens. Token and spend limits count it; request-count limits and
the key's limits do not; it takes a place among the calls in flight. A repeated text within
`cache_seconds` costs nothing.

**Tuning a catalogue.** The workspace's **Plugins** page has two panels. **Try the intent
plugin** classifies a text with each engine chosen and shows the label, its confidence, the
flags and what the rules would do (`POST …/plugins/intent/try`). **Evaluate a labelled set**
takes up to 200 lines of `intent<TAB>text` (or JSON objects with `text` and `intent`) and shows
each engine's accuracy, precision and recall per intent and the wrong answers
(`POST …/plugins/intent/evaluate`). Both make real engine calls, without the cache. Use them to
compare engines and catalogue versions before switching a binding to `enforce`.

Both panels start with demonstration data for the catalogue the binding names
(`GET …/plugins/intent/demo`): a text a block rule acts on, and two samples per intent.
Intents the built-in template also has get its labelled texts, written apart from its
examples. Other intents get the catalogue's own examples, which the LLM engine sees in its
prompt, so it finds them more easily than real calls; the panel says which. Replace the demo
with texts of your own before judging an engine.

**Using the intent.** Routing rules match it (`when.intent`, CEL `intent`,
`intent_confidence`, `flags`); limits count only some intents (`filter.intents`). Both read
only what an enforcing binding found.

**Paths a plugin cannot read.** While a binding enforces, a key set to blind relay is
refused (`403 not_allowed`), and so are Anthropic message batches. Embeddings, speech and
token counts are not classified.

**Upgrading.** The usage events gained fields for plugins (ClickHouse `0010_plugins`): roll
out the analytics service before the core (section 6).

### The response cache

The response cache ([plugin concept](plugin-concept.md), section 15) answers a call that was
answered before with the kept answer, without asking the provider: the same model was sent
the same request, as the gateway shaped it. A stream and a whole answer share one kept
answer, and it is written in the caller's dialect. The core installs it on its first start
with a default binding for every workspace, in the mode `PGW_CACHE_DEFAULT_MODE` gives: `off`
unless `deploy/.env` says `shadow` or `enforce` (the development stack and the demo say
`enforce`). Switch it on in **Platform › Plugin installs** by setting the default binding's
mode, best to `shadow` first to see the hit rate; or bind it for one organisation or
workspace on its **Plugins** page. An install that was changed, switched off or deleted stays
so, whatever `PGW_CACHE_DEFAULT_MODE` says later.

**What is cached.** By default calls with temperature 0: the temperature the provider is
sent, or, for a model that takes none (most models that reason), the one the caller asked
for. Not: proxy mode, embeddings, speech, token counts, batches, the gateway's own calls,
requests for several choices or log-probabilities. Not kept: refused or failed calls, cut
streams, answers that ended for another reason than stop, length or tool calls, answers with
a provider's own tools (a web search), and answers above `max_answer_bytes`.

**Settings**, at three levels: the platform (the install's **default binding**), an
organisation's binding (which can lock its workspaces), a workspace's binding.

| Setting | Means |
|---|---|
| `mode` | `enforce` answers from the cache (the default binding's). `shadow` looks and records `would_hit`, but every call reaches its provider: use it to see the hit rate first. `off` switches it off for the scope. |
| `settings.calls` | `deterministic` (the default) or `all`: every call, whatever its temperature. |
| `settings.scope` | Whose calls share answers. `key` (the default): the calls of one API key. `end_user`: the calls of one end user of one key (`x-gw-end-user`, or the dialect's own field); a call that names none is not cached. `workspace`: every caller of the workspace. |
| `settings.ttl_seconds` | How long an answer is kept (3600, at most 604800). |
| `settings.max_answer_bytes` | Larger answers are not kept (1048576). |
| `timeout_ms` | How long a call waits for the store before it goes on without it (100). `on_error`, `expose` and `priority` do not apply. |

**Callers** choose per call with `x-gw-cache: off` (neither answered from the cache nor
kept), `refresh` (a fresh answer, which is kept) or `on`; on the native interface also
`gateway.cache.response`. Every response says what the cache did in `x-gw-cache`: `hit`,
`miss`, `would_hit`, `refresh` or `bypass` (not a call the cache takes). A hit also has `Age`
(seconds since the answer was kept) and `x-gw-attempts: 0`.

**What a hit costs.** Nothing: no provider call, no tokens, cost 0, no key slot, and no
limit counts it. Its usage event has `cache: hit`, `usage_source: cache` and
`cache_saved_cost`, what the kept answer would cost now; reports filter and group by `cache`.

**The store.** `gw-valkey`, shared by every gateway (`PGW_VALKEY_URL`, `PGW_VALKEY_PASSWORD`):
memory only, nothing on disk, the least recently used answers out first when it holds
`PGW_VALKEY_MAXMEMORY` (384 MB). A gateway without `PGW_VALKEY_URL` keeps answers in its own
memory (`PGW_CACHE_MEMORY_MB`, 64). A store that fails is a miss ("The response cache fails",
section 10). With Helm, Valkey is not part of the chart: set `core.valkeyUrl` and
`secrets.valkeyPassword` to one of your own, configured without persistence and with
`maxmemory-policy allkeys-lru`, and reach it with `rediss://` (`core.valkeyCaFile` for a
certificate of your own authority).

**Sealed.** Each answer reaches Valkey sealed with AES-256-GCM under a key derived from
`PGW_KEK` for its workspace, and under a name that is a keyed hash of the request: whoever
reads Valkey sees workspace ids, how many answers each has and their sizes, never what was
asked or answered. An answer that does not open is a miss, so answers written into Valkey by
anyone but the gateways are never served. A rotation of `PGW_KEK` costs one round of misses
(section 8). Gateways on other machines reach Valkey with TLS (section 9); a gateway that
reaches a Valkey on another host with plain `redis://` warns at start.

**Data.** Answers are kept per workspace and never answered to another. Within a workspace
the scope decides who shares them: by default a key gets only the answers its own calls were
given, and with `workspace` a caller who sends exactly another caller's request gets the
other's answer. The end user is what the application names, so `end_user` keeps apart the
users of an application that names them; it is no boundary between callers who hold the same
key. The audit rule
`forbid`, of the platform or of an organisation, switches the cache off for every workspace it
reaches, whatever the bindings say; their Plugins pages and **Platform › Response cache** say
so. Answers kept before the rule was set are no longer read and expire in their time; clear the
scope to remove them at once. A workspace that must keep nothing without such a rule switches
its binding off.

**Load tests** send `x-gw-cache: off`, so that they measure the gateway and its providers; a
test that names the header itself measures the cache.

**Clearing.** A clear makes every answer kept before it a miss: the next calls are answered by
their providers, at their cost, and their answers are kept again. Clear when kept answers are
known to be wrong: a model changed behind its id, a provider answered badly, a template's
model was swapped for one that should answer otherwise.

| Clears | Where | Who |
|---|---|---|
| Every workspace | **Platform › Response cache**, "Clear every workspace" (`POST /response-cache/clear`) | Platform admins |
| An organisation's workspaces | The organisation's **Plugins** page (`POST /orgs/{org}/response-cache/clear`) | Its admins |
| One workspace | The workspace's **Plugins** page (`POST /orgs/{org}/workspaces/{ws}/response-cache/clear`) | Its admins |

- A clear is a configuration change (`response_cache_clear`, audited as `clear`): every
  instance has it within a second or two, and the answer's `x-gw-applied` says how many have
  it already.
- An answer counts as kept before the clear when its call began no later than two seconds
  after it, which allows for clocks a little apart. Keep the hosts' clocks in sync (NTP): a
  gateway whose clock runs further ahead keeps answering from answers kept just before.
- The space is freed as well. With Valkey the instance that cleared removes the scope's keys
  in the background (the answer says `reclaim: started`; its log says how many went). A
  gateway that keeps answers in its own memory drops them when it applies the clear
  (`per_instance`).
- A clear outlives switching the plugin off and on, and deleting and installing it again.
  Restarting `gw-valkey` empties the cache too, for every workspace at once.

**Seeing what it does.**

| Where | What | Who |
|---|---|---|
| **Insight › Usage › Response cache** | For a range and scope: the hit rate (hits over the calls looked up), the calls answered from the cache and in shadow mode, what they saved in the reporting currency (hits without an exchange rate in their own), over time, by model and by workspace | Every role, within its scope |
| **Platform › Response cache** | The store and what it holds (Valkey's keys, memory and eviction policy, or each gateway's own memory), each instance's answers and breaker, the platform's binding, the latest clears of every scope | Platform roles; clearing for platform admins |
| An organisation's or workspace's **Plugins** page | Whether the cache answers its calls, from whose binding, with which settings, why none does, and its last clear | Readers of the scope; clearing for its admins |
| **Insight › Monitoring › Metrics**, "Gateway internals" (and Grafana's) | What the cache did per minute, the hit rate, the time of a lookup, the store's errors | Platform roles |

## 19. The shared networks pacod-net and pacod-admin-net

pacod-gw is one of several pacod products (pacod-id, pacod-ap, pacod-cms, pacod-kb). They
follow one convention, `pacod-demo/docs/NETWORK.md` in the pacod-demo repository, so that each
runs on its own and all of them run together as one stack (pacod-demo) without editing their
files:

- **Two networks.** Every service joins the external Docker network `pacod-net`. The
  listeners that only servers reach, and their clients, also join the external network
  `pacod-admin-net`, which is internal (no route out): the management API, bound to
  `pacod-gw-admin:8081`, and pacod-id's internal listener, bound to `pacod-id-internal:8071`
  (section 2). Each alias exists on `pacod-admin-net` alone, so a listener bound to it is not
  on `pacod-net`. The overlays that span machines or isolate the load agent keep their extra
  networks (`hub`, `lb`, `load-gw`, `load-out`; `deploy/node/` and `deploy/load-agent/` keep
  their own). `make network` creates both networks, and stops when `pacod-admin-net` exists
  without being internal, which compose does not check; every `up` target runs it first, and
  `down` keeps them.
- **One name per service.** The service, its container and its name on the network are the
  same. Components that several products use are named `pacod-*` (`pacod-postgres`,
  `pacod-id`, `pacod-caddy`, `pacod-prometheus`, ...), and only one of each runs on a host;
  the gateway's own services are named `gw-*`, the core `pacod-gw`. The instance ids
  (`core`, `gateway-1`, ...) and the Prometheus job names (`pacod-gw-core`, `otel-collector`)
  did not change.
- **Two compose files.** `deploy/docker-compose.yml` holds the gateway's own services;
  `deploy/docker-compose.standalone.yml` holds its copies of the shared components. This
  runbook always loads both. The full stack loads only the first and runs the shared
  components once, for every product.
- **Fragments.** The gateway's share of the shared configuration lives in `platform/`, and
  both its own stack and the full stack read it from there:

| File | What | Read by |
|---|---|---|
| `platform/caddy/gw.caddy` | The API and console sites, with their snippets (`gw_*`). Addresses from `GW_API_SITE` and `GW_CONSOLE_SITE`. | `import /etc/caddy/sites/*.caddy` in `deploy/caddy/Caddyfile` and `Caddyfile.dev` |
| `platform/postgres/gw-db.sh` | Creates the role `pacod_gw` (not a superuser) and its database where missing, and sets the role's password to `GW_DB_PASSWORD`. Refuses an installation where `pacod_gw` is still the superuser the image created (section 6). | `pacod-db-init`, at every start, as the superuser `postgres` |
| `platform/prometheus/gw.yml` | The scrape jobs `pacod-gw-core` and `otel-collector`. | `scrape_config_files` in `deploy/prometheus/prometheus.yml` |

**What it costs.** The databases used to sit on a network without a route in or out of the
host (`backend`), and pacod-id's public listener on a network of its own with Caddy. On
`pacod-net` every container reaches every other, and all have a route out; only the two
listeners on `pacod-admin-net` are kept from it. That network is shared too: every server on
it, whichever product it belongs to, reaches both of them, where Helm's network policies
admit each listener's own clients only. Only Caddy publishes
ports, as before. Two stacks that both run a shared component (this one and another product's
own stack, or this one and the minimal gateway of section 17) do not run on the same host at
the same time.

### Upgrading an installation to the shared network

The services and volumes were renamed. Compose would start the renamed services with new,
empty volumes and leave the data in the old ones, so move it once, before the first start of
the new release (section 6, step 5):

| Old volume | New volume |
|---|---|
| `pacod-gw_postgres-data` | `pacod-gw_pacod-postgres-data` |
| `pacod-gw_clickhouse-data` | `pacod-gw_gw-clickhouse-data` |
| `pacod-gw_spool` (`spool-1`, `spool-2` with the scale layout) | `pacod-gw_gw-spool` (`gw-spool-1`, `gw-spool-2`) |
| `pacod-gw_tester-data` | `pacod-gw_gw-tester-data` |
| `pacod-gw_caddy-data`, `pacod-gw_caddy-config` | `pacod-gw_pacod-caddy-data`, `pacod-gw_pacod-caddy-config` |
| `pacod-gw_prometheus-data` | `pacod-gw_pacod-prometheus-data` |
| `pacod-gw_alertmanager-data` | `pacod-gw_pacod-alertmanager-data` |
| `pacod-gw_grafana-data` | `pacod-gw_pacod-grafana-data` |

1. Stop the stack with the compose files of the old release (`make down`, or `scale-down`
   or `hub-down`, before `git pull`), and make a backup (section 7).
2. Get the new release, create the network and copy every volume that exists:

   ```sh
   docker network inspect pacod-net >/dev/null 2>&1 || docker network create pacod-net
   docker network inspect pacod-admin-net >/dev/null 2>&1 || docker network create --internal pacod-admin-net
   for pair in postgres-data:pacod-postgres-data clickhouse-data:gw-clickhouse-data \
       spool:gw-spool spool-1:gw-spool-1 spool-2:gw-spool-2 tester-data:gw-tester-data \
       caddy-data:pacod-caddy-data caddy-config:pacod-caddy-config \
       prometheus-data:pacod-prometheus-data alertmanager-data:pacod-alertmanager-data \
       grafana-data:pacod-grafana-data; do
     key="${pair#*:}" old="pacod-gw_${pair%%:*}" new="pacod-gw_${pair#*:}"
     docker volume inspect "$old" >/dev/null 2>&1 || continue
     docker volume create --label com.docker.compose.project=pacod-gw \
       --label com.docker.compose.volume="$key" "$new" >/dev/null
     docker run --rm -v "$old:/from:ro" -v "$new:/to" busybox:1.38.0 cp -a /from/. /to/
   done
   ```

   `cp -a` keeps owners and modes, which PostgreSQL and ClickHouse check. The labels make the
   new volumes compose's own, as if it had created them.
3. In `deploy/.env`, rename the service names that values may hold: `PGW_DATA_UPSTREAMS`
   (`gateway-1:8080` is now `gw-gateway-1:8080`, `core:8080` is `pacod-gw:8080`),
   `TESTER_LOAD_AGENTS` (`load-agent` is `gw-load-agent`), `TESTER_LOAD_MOCK_URL` and
   `PGW_EGRESS_ALLOW_PRIVATE` (`load-mock` is `gw-load-mock`), `LOAD_AGENT_TARGET_URL`
   (`core` is `pacod-gw`, `caddy` is `pacod-caddy`). Then set `GW_DB_PASSWORD` and a new
   `POSTGRES_PASSWORD` as section 6 says ("Upgrading to the release where the core is not
   PostgreSQL's superuser").
4. Run `deploy/scripts/demote-db-role.sh` (section 6), which starts the stack; with the
   scale or hub layout, set `COMPOSE_FILE` first. The console, Prometheus and Grafana show the
   history of before.
5. Once everything is as before, remove the old volumes:
   `docker volume rm pacod-gw_postgres-data pacod-gw_clickhouse-data ...`.

This procedure has not been exercised on a production installation. A gateway on another
machine (`deploy/node/`) keeps its volume name and needs nothing but the new release.
