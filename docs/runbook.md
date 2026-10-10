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
| `postgres` | Configuration, encrypted provider credentials, audit log, limit counters. | volume `postgres-data` |
| `clickhouse` | Usage events, audit-mode records, rollups. | volume `clickhouse-data` |
| `caddy` | TLS and the only entry from outside. | volumes `caddy-data`, `caddy-config` (certificates) |
| `otel-collector` | Receives traces from the core. | nothing |
| `prometheus` | Scrapes the core's metrics, evaluates the alert rules. | volume `prometheus-data` |
| `alertmanager` | Sends the alerts by email and to a webhook (section 10). | volume `alertmanager-data` |
| `grafana` | Dashboards. | volume `grafana-data` |
| `preflight` | Runs once at every start and checks that the secrets are set. | nothing |

Requests keep flowing when PostgreSQL, ClickHouse or the analytics service is down. The
core works from the last configuration it loaded and collects events in its spool.

With the scale layout, `core` serves only the management API and the services `gateway-1`,
`gateway-2` serve the data plane, each with a spool volume of its own; gateways on other
machines run `deploy/node/` (section 9).

## 2. Ports

Only Caddy publishes ports on the host.

| Host port | Variable | Serves |
|---|---|---|
| 80 | `PGW_HTTP_PORT` | Redirect to HTTPS, and the certificate challenge. |
| 443 (TCP and UDP) | `PGW_HTTPS_PORT` | The API host and the console host. |

Inside the compose network:

| Service | Port | Serves | Reachable from outside |
|---|---|---|---|
| `core` | 8080 | Data plane: `/v1`, `/gw/v1`, `/proxy`, `/healthz`, `/readyz` | Yes, through Caddy on the API host. |
| `core` | 8081 | Management API: `/api/v1` | No. The console calls it inside the network. |
| `core` | 9100 | `/metrics` | No. |
| `analytics` | 8090 | Event ingest and reports | No. |
| `ui` | 3000 | Admin console | Yes, through Caddy on the console host. |
| `tester` | 8095 | The tester's API, for the console | No. |
| `postgres` | 5432 | PostgreSQL | No. |
| `clickhouse` | 8123 | ClickHouse HTTP interface | No. |
| `otel-collector` | 4318, 4317, 13133, 8888 | OTLP over HTTP, OTLP over gRPC, health, own metrics | No. |
| `prometheus` | 9090 | Prometheus | No. |
| `alertmanager` | 9093 | Alertmanager | No. |
| `grafana` | 3000 | Grafana | Only for allow-listed addresses, at `/grafana/` on the console host. |
| `caddy` | 2019 | Caddy's admin endpoint, loopback only | No. |

PostgreSQL and ClickHouse sit on a network without a route to the host (`backend`).

Caddy also listens on 8088 inside the stack: the balanced data plane without TLS, for the
tester of the scale layout. It is not published.

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
2. Do the same for `PGW_KEY_PEPPER`.

The reason for the separation: a database backup together with the key gives access to
every provider credential. Apart, neither is of use to someone who obtains it. The backup
script does not copy `deploy/.env` and refuses to run until you confirm that the key is
stored elsewhere (section 7).

## 4. First deployment

You need a Linux host with Docker Engine and the compose plugin, 8 GB of memory, and two
DNS names that point to it: one for the API and one for the console.

**Step 1. Get the code.**

```sh
git clone <repository> pacod-gw && cd pacod-gw
```

**Step 2. Get the images.** Either build them on the host:

```sh
docker build -f deploy/docker/rust.Dockerfile --target core      -t pacod-gw-core:0.1.0 .
docker build -f deploy/docker/rust.Dockerfile --target analytics -t pacod-gw-analytics:0.1.0 .
docker build -f deploy/docker/ui.Dockerfile                      -t pacod-gw-ui:0.1.0 .
```

(`make images` runs these three commands and builds the tester's image as well), or, if
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
```

Then set, in the same file:

| Variable | Set it to |
|---|---|
| `PGW_API_HOST` | The DNS name of the API, for example `api.example.com`. |
| `PGW_CONSOLE_HOST` | The DNS name of the console, for example `console.example.com`. |
| `PGW_TLS` | Your e-mail address. Caddy then obtains certificates from Let's Encrypt. Leave `internal` only for a test without real DNS names. |
| `PGW_ADMIN_EMAIL` | The e-mail address you will log in with. |
| `PGW_KEK_ID` | A name for the key, for example the month: `2026-10`. |
| `GRAFANA_ROOT_URL` | `https://<console host>/grafana/` |

**Step 4. Store `PGW_KEK` and `PGW_KEY_PEPPER` somewhere else** (section 3). Do it now.

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
`PGW_ADMIN_PASSWORD`, and change the password.

With `PGW_TLS=internal`, clients do not trust the certificate. Either pass the root
certificate to them (`dc exec caddy cat /data/caddy/pki/authorities/local/root.crt`) or use
`curl -k` for a test.

**Step 8. Set up the gateway in the console.** Add a provider and its API key, activate a
model, create an organisation and a workspace, and create a gateway key. Applications then
call `https://api.example.com/v1` with that key.

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
   other secrets may be new ones.
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
| `PGW_KEY_PEPPER` | Do not rotate it. Changing it invalidates every gateway key and admin token at once. If it has leaked, change it and issue all keys again. |
| `GRAFANA_ADMIN_PASSWORD` | Change the password in Grafana. The variable is read only when Grafana creates its database. |
| TLS certificates | Caddy renews them. Nothing to do. |

## 9. Scaling

The design target for one core instance with 2 CPUs and 1 GB is 10 000 concurrent streams
in proxy mode and 5 000 in managed mode. A load test has not confirmed these numbers yet.
Look at your own numbers before adding instances: the dashboard "Gateway internals" shows
streams in flight, rejections with reason `gateway_overloaded`, and the gateway delay.
Several instances also let you upgrade without a gap in the data plane.

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

### Logging in to the console fails

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

### A provider or model fails

Alerts: `GatewayErrorRateHigh`, `GatewayTimeoutsSpike`, `GatewayAllCallsFailing`.

1. Grafana, "Providers and models": is it one model, one provider, or everything?
2. Console, Requests, filter by the provider and status: the provider's own error message
   is stored with each failed call.
3. One provider, all calls, HTTP 401 or 403: the provider key is wrong or revoked. Rotate
   it (section 8).
4. HTTP 429 from the provider: its rate limit or quota. In managed mode the gateway retries
   before the first byte and then reports the failure; proxy mode passes the 429 on.
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
   `cached_tokens_at` and `reasoning_tokens_at`. `extra_params` lists the request members
   managed mode passes on to it. A change applies to the next call.
9. The request log shows `model_reported` different from the model asked for: the provider
   answered with another model. When that model is in the repository, the call is
   reported and priced as that model.

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
  ends the run's management calls; start it again.
- *Left over* on a run: clean-up could not delete these paths, for example after the tester
  was stopped during a run (the run then shows as *interrupted*). Delete the scratch
  organisations named `test-<run>-<suite>` in the console: first their keys, templates,
  limits, grants and routing schemes, then the workspace, then the organisation.
- Mock mode keeps the providers `tester-mock-openai` and `tester-mock-anthropic` and their
  two models on purpose; they are reused by every run.

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
secret has a public development default.

```sh
docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.dev.yml up -d --build --wait
```

`make dev-up` and `make dev-down` do the same.

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

Console login: `admin@example.com`, password `pacod-gw-dev`. To send requests to the mock
provider, add a provider in the console with the base URL `http://gw-mock:9000`.

The ports are bound to `127.0.0.1`. Never use the override file on a machine that others
can reach. To run a binary directly instead of its container, see `.env.example` in the
repository root.

Stop it with `docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.dev.yml down`.
Add `-v` to delete the data as well.

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
`flex`). A rate or a variant the current price does not have cannot be measured as a
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
again. A repository model the provider no longer lists is flagged; nothing is retired by
itself. Platform, Providers, Test asks one provider for its list at once, which tests its
address and its shared credential.

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
| Users, admin tokens, role bindings | Create them again. |
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

The links carry their token in the URL. Caddy removes the `token` and `next` parameters
(after signing in, `next` can lead back to an invitation link) and the `Referer` header
from its logs; a proxy in front of Caddy should do the same.

## 14. Members and invitations

An organisation's admins manage who works in it, on the organisation's **Members** page or
with `/api/v1/orgs/{org}/members` and `/api/v1/orgs/{org}/invitations`. They give a member
one role in the organisation or in one of its workspaces, change it, and remove members. A
change that would leave the organisation without an organisation admin is refused, unless a
platform admin makes it. Workspace admins see their workspace's members but do not change
them. Platform admins can still bind any role directly under **Platform → Access**.

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

## 15. Running with make

The `Makefile` in the repository root wraps the commands of this runbook. Run every target
from the repository root, as `make <target>`. The targets that start services need Docker
with the compose plugin. The development targets also need Rust (`cargo`), Node.js (`npm`,
with `npm ci` done once in `ui/`) and `uv` for the tester.

### Running an installation

| Target | What it does | Details |
|---|---|---|
| `make images` | Builds the four images, tagged `0.1.0`. `make images VERSION=0.2.0` builds another tag; it must equal `PGW_VERSION` in `deploy/.env`. | Section 4 |
| `make up`, `make down` | Starts or stops the stack with one core. | Sections 1 and 4 |
| `make scale-up`, `make scale-down` | The same with the management API on its own and two gateways. | Section 9 |
| `make hub-up`, `make hub-down` | On the central host when gateways on other machines join: the scale layout plus the hub file. | Section 9 |
| `make node-up`, `make node-down` | On another machine: one gateway that joins the central host. | Section 9 |
| `make backup` | Backs up PostgreSQL and ClickHouse (`deploy/scripts/backup.sh`). | Section 7 |

The targets read their settings from `deploy/.env`; `node-up` and `node-down` read them from
`deploy/node/.env`. `up` waits until every service is healthy and fails if one does not
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
| `make dev-up`, `make dev-down` | The development stack (section 11): images built from the working tree, the mock provider, ports between 10000 and 19999, no `deploy/.env` needed. `DEV_HTTPS=1` adds ports 80 and 443 with `https://console.localhost` and `https://api.localhost`. |
| `make dev-scale-up`, `make dev-scale-down` | The same in the scale layout: Caddy on 18080, the gateways on 18082 and 18083. `DEV_HTTPS=1` as above. |
| `make node-dev-up`, `make node-dev-down` | A gateway that plays another machine, next to the development stack (section 11). |
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
| `make tester-test` | The tester's lint, format check and tests. |
| `make deny` | Licences, banned crates, sources and advisories of the Rust dependencies, by `deny.toml`. Needs `cargo install cargo-deny --locked`. CI runs it too. |

### Against a running stack

| Target | What it does |
|---|---|
| `make smoke-mock` | The whole flow against the mock provider: providers, models, both modes, both dialects, limits, routing, audit mode, the price tracker, Anthropic files and batches, key revocation. |
| `make smoke-live` | A few tiny calls per provider with the keys in `.env` (a fraction of a cent), a tiny file and a one-item batch at Anthropic (the batch is cancelled at once), then a scan of the logs for those keys. |
| `make load` | Calls through the gateway against the mock, compared with calling the mock directly. `PROFILE=managed` for managed mode, `LOAD_ARGS="--requests 20000 --concurrency 500"` for more. |

They go to the development stack by default: the management API on 18081, the data plane on
18080, which is Caddy in the scale layout. `SMOKE_ADMIN_URL`, `SMOKE_DATA_URL` and
`SMOKE_MOCK_URL` point them elsewhere; `PGW_ADMIN_EMAIL` and `PGW_ADMIN_PASSWORD` are the
login. With a gateway from `make node-dev-up` among the upstreams, set
`SMOKE_MOCK_URL=http://host.docker.internal:19000`: the node cannot reach the mock under its
name in the stack.
