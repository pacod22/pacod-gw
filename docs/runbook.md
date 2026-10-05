# pacod-gw runbook

This document is for the person who installs and runs pacod-gw. It assumes Docker and a
shell, and no knowledge of the project. The design is described in
[ai-gateway-concept.md](ai-gateway-concept.md); you do not need it to operate the stack.

**State of this document.** The deployment files were checked without running the whole
stack (`make deploy-check` repeats those checks): the configuration of every component was
validated with its own tool, the images of the core and the analytics service build,
PostgreSQL and ClickHouse start and become healthy as configured, and the backup and
restore scripts were run against those two on their own. A first deployment of the
complete stack has not been done yet. Where a step depends on behaviour of the gateway
that could not be exercised, the text says so.

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

All commands are run from the repository root. `dc` stands for:

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
| `postgres` | Configuration, encrypted provider credentials, audit log, limit counters. | volume `postgres-data` |
| `clickhouse` | Usage events, audit-mode records, rollups. | volume `clickhouse-data` |
| `caddy` | TLS and the only entry from outside. | volumes `caddy-data`, `caddy-config` (certificates) |
| `otel-collector` | Receives traces from the core. | nothing |
| `prometheus` | Scrapes the core's metrics, evaluates the alert rules. | volume `prometheus-data` |
| `grafana` | Dashboards. | volume `grafana-data` |
| `preflight` | Runs once at every start and checks that the secrets are set. | nothing |

Requests keep flowing when PostgreSQL, ClickHouse or the analytics service is down. The
core works from the last configuration it loaded and collects events in its spool.

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
| `postgres` | 5432 | PostgreSQL | No. |
| `clickhouse` | 8123 | ClickHouse HTTP interface | No. |
| `otel-collector` | 4318, 4317, 13133, 8888 | OTLP over HTTP, OTLP over gRPC, health, own metrics | No. |
| `prometheus` | 9090 | Prometheus | No. |
| `grafana` | 3000 | Grafana | Only for allow-listed addresses, at `/grafana/` on the console host. |
| `caddy` | 2019 | Caddy's admin endpoint, loopback only | No. |

PostgreSQL and ClickHouse sit on a network without a route to the host (`backend`).

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

Rules:

- `POSTGRES_PASSWORD` and `CLICKHOUSE_PASSWORD` may contain only letters, digits and
  `. _ ~ -`. The PostgreSQL password becomes part of a URL. `openssl rand -hex` produces such
  values.
- Provider API keys are not in this file. They are entered in the console and stored
  encrypted in PostgreSQL.

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

(`make images` runs the same three commands), or, if your organisation publishes them to a
registry, set `PGW_REGISTRY` in step 3 (with
the trailing slash, for example `registry.example.com/platform/`) and Docker pulls them.
The tag must equal `PGW_VERSION`.

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
appears with a private address.

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
| `core` | `GET :8080/readyz` | A configuration snapshot is loaded and the spool accepts events. | PostgreSQL unreachable at start, a missing secret, or the spool is full while the policy is `fail_closed`. |
| `analytics` | `GET :8090/readyz` | The ClickHouse schema is in place. | ClickHouse unreachable or wrong password. The service keeps retrying. |
| `ui` | `GET :3000/` | The console answers. | The process crashed; see `dc logs ui`. |
| `caddy` | its admin endpoint | Caddy runs with a loaded configuration. | An error in the Caddyfile or in a variable it uses. |
| `prometheus` | `GET /-/ready` | Prometheus has loaded its data. | A broken rule file, or the volume is full. |
| `grafana` | `GET /api/health` | Grafana and its database are up. | First start still running (up to five minutes are allowed). |

The two endpoints of the core and the analytics service differ on purpose:

- `/healthz` answers as soon as the process runs. It says nothing about dependencies. A
  failure means the process hangs or is gone.
- `/readyz` answers 200 only when the service can do its work. The core's data plane
  answers 503 with `"status": "event spool full"` when it refuses requests. The management
  API (port 8081) answers 503 with `"status": "database unreachable"` when it cannot reach
  PostgreSQL. With the role `admin` the core serves both endpoints on 8081 instead of 8080.

A load balancer in front of several instances should use `/readyz`. Caddy does.

The `otel-collector` has no health check in compose because its image contains no tool to
run one. Its own endpoint is `http://otel-collector:13133/` inside the network.

## 6. Upgrade

1. Read the release notes of the new version.
2. Make a backup (section 7).
3. Get the new images: build them with the new tag, or let Docker pull them.
4. Set `PGW_VERSION` in `deploy/.env` to the new tag.
5. Apply:

   ```sh
   git pull            # the compose file, Caddyfile, dashboards and rules belong to the release
   dc pull --ignore-pull-failures   # images built on this host cannot be pulled; that is fine
   dc up -d --wait
   ```

Compose replaces only the containers whose image or settings changed. The core migrates
the PostgreSQL schema when it starts, and the analytics service migrates ClickHouse. No
separate migration step is needed. To apply the PostgreSQL migrations without starting the
gateway, run `dc run --rm core migrate`.

During the replacement of the core the data plane is down for the time the new container
needs to become ready. Running streams get up to 45 seconds to finish. For an upgrade
without that gap, run several gateway instances (section 9).

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

## 8. Rotating keys

### Provider keys

No restart is needed. In the console: Platform, Providers, the provider, Credentials, add a
credential and make it current. The next request uses the new key. The previous one stays
valid for a grace period and is then disabled, so calls already running are not affected.
Revoke the old key at the provider after the grace period.

The same through the management API (see section 4 for how to reach it):

```sh
curl -X POST http://core:8081/api/v1/providers/<provider id>/credentials \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H "content-type: application/json" \
  -d '{"label":"2026-10 rotation","secret":"<new provider key>","make_current":true}'
```

The answer shows the new configuration version and on how many instances it is applied.

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

3. Restart the core: `dc up -d --wait core`. From now on every credential that is added or
   rotated is encrypted under the new key. Existing ones are still read with the old key.

4. Re-encrypt the existing credentials. The first release has no single command for this.
   A credential is re-encrypted when it is entered again: rotate each provider credential
   as described above, with the same or a new provider key. The list of credentials shows
   for each one the `kek_id` it is encrypted under.

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

What several instances share and what they do not:

- They share PostgreSQL. A configuration change made on one instance reaches the others
  within about two seconds. The console shows "applied on N of N instances".
- Each instance has its own spool and must have its own volume. Two instances must never
  write to the same spool directory.
- Limits are counted per instance and combined through PostgreSQL once a second. A limit
  can be overshot by the calls in flight plus one second of traffic.
- Only the data plane is scaled. Run the management API as one or two instances with the
  role `admin`, off the public network.

### On Kubernetes

Use the Helm chart (`deploy/helm/pacod-gw`, see its README). The gateway is a StatefulSet;
set `gateway.replicas`. Each instance gets its own spool volume, the admin role runs as its
own Deployment, and the Ingress balances across the instances that answer `/readyz`.

### With compose, on one host or a few

The compose file runs a single core with the role `all`. For several instances, add an
override file. The merged file below was checked with `docker compose config`; the layout
itself has not been run yet, so test it before production.

```yaml
# deploy/docker-compose.scale.yml
services:
  core:                       # becomes the management API only
    environment:
      PGW_ROLE: admin
    healthcheck:
      test: ["CMD", "/usr/local/bin/healthcheck", "http://127.0.0.1:8081/readyz"]

  gateway-1:
    extends: { file: docker-compose.yml, service: core }
    environment:
      PGW_ROLE: gateway
      PGW_INSTANCE_ID: gateway-1
    volumes:
      - spool-1:/var/lib/pacod-gw/spool
    depends_on:
      core: { condition: service_healthy }

  gateway-2:
    extends: { file: docker-compose.yml, service: core }
    environment:
      PGW_ROLE: gateway
      PGW_INSTANCE_ID: gateway-2
    volumes:
      - spool-2:/var/lib/pacod-gw/spool
    depends_on:
      core: { condition: service_healthy }

  analytics:
    environment:
      PGW_CORE_DATA_URL: http://gateway-1:8080

volumes:
  spool-1:
  spool-2:
```

Then:

1. Set `PGW_DATA_UPSTREAMS=gateway-1:8080 gateway-2:8080` in `deploy/.env`. Caddy sends each
   request to the instance with the fewest calls in flight and skips instances that do not
   answer `/readyz`.
2. In `deploy/prometheus/prometheus.yml`, list `gateway-1:9100` and `gateway-2:9100` next to
   `core:9100`.
3. Check the merged file and start it:
   `docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.scale.yml config --quiet`,
   then the same with `up -d --wait`.

Check that each gateway service ended up with exactly one spool volume
(`docker compose ... config | grep -A3 spool`). Upgrade the instances one after the other
(`up -d --wait gateway-1`, then `gateway-2`), so the data plane stays available.

For instances on several hosts, put a load balancer in front that checks `/readyz`, point
every instance at the same PostgreSQL and the same analytics service, and give each a
distinct `PGW_INSTANCE_ID`.

### The stores

PostgreSQL carries configuration and counters, not request payloads; its load is small.
ClickHouse grows with the number of requests. Watch the disk of the `clickhouse-data`
volume and set a retention for usage events and audit-mode records in the console.

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
| What is failing right now? | Console: Monitoring. Prometheus alerts: `http://prometheus:9090/alerts` inside the network, or Grafana. |
| What happened to one request? | Console: Requests. Search by the `x-gw-request-id` response header. |
| Who changed the configuration? | Console: System, audit log. |
| Trends | Grafana: Overview; Providers and models; Workspaces; Gateway internals. |
| Traces | The collector logs a summary (`dc logs otel-collector`). For full traces, configure a backend in `deploy/otel-collector/config.yaml`. |

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

### A provider or model fails

Alerts: `GatewayErrorRateHigh`, `GatewayTimeoutsSpike`, `GatewayAllCallsFailing`.

1. Grafana, "Providers and models": is it one model, one provider, or everything?
2. Console, Requests, filter by the provider and status: the provider's own error message
   is stored with each failed call.
3. One provider, all calls, HTTP 401 or 403: the provider key is wrong or revoked. Rotate
   it (section 8).
4. HTTP 429 from the provider: its rate limit or quota. The gateway retries before the
   first byte and then reports the failure.
5. Timeouts: the provider is slow, or the timeout of the template is too short for the
   model. The gateway never retries a timeout, because the provider may have completed and
   billed the call.
6. Everything fails for every provider: check outbound connectivity and DNS from inside the
   stack. `dc exec caddy nslookup api.openai.com` must resolve, and
   `dc exec caddy wget -S --spider https://api.openai.com/ 2>&1 | head -3` must show an
   HTTP status line. Any status, even 401 or 404, proves that the provider is reachable.
7. A provider on a private address (a local model server) is refused unless it is listed
   in `PGW_EGRESS_ALLOW_PRIVATE`.

### Requests are rejected

Alert: `GatewayRejectionSpike`.

The label `reason` says why. Grafana, "Gateway internals", panel "Rejections by reason".

- A rate limit or a quota: a workspace reached a limit. Console, the workspace, Limits,
  shows each limit and how much of it is used. Raise the limit there if that is intended.
  It takes effect with the next request. There is no alert for a limit that is nearly used
  yet: the core does not export that figure as a metric.
- `gateway_overloaded`: the instance reached its cap of calls in flight. Add instances
  (section 9) or raise `[limits] max_in_flight` in the bootstrap file.
- Authentication failures: an application uses a revoked or mistyped gateway key.
- HTTP 413 from Caddy: the body is larger than `PGW_MAX_BODY`. The core has its own limit
  (`[limits] max_body_bytes`, 32 MiB by default); raise both.

### Prices are missing or stale

Alert: `GatewayPriceMissing`. There is no alert for a stale price yet: the core does not
export the age of a price as a metric. Check the Pricing page of the console for that.

A call to a model without a price is settled without a cost, so spend reports and spend
caps undercount. Console, Platform, Pricing: enter the price, mark the model as free, or
approve the open proposal. For a stale price, check the last run of the price job on the
same page. The job runs in the analytics service and needs outbound access to its sources.
Grafana, "Providers and models", panel "Calls settled without a price", shows which models
are affected and since when.

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
time are incomplete, limits are not affected. With `full_policy = "fail_closed"` the core
refuses requests instead and `/readyz` answers 503.

Audit-mode records have their own spool (4 GiB). When it is full, calls that require a
record are refused with `503 audit_unavailable`; others proceed without a record.

Do not delete the `spool` volume to "fix" a backlog. It holds the events that have not been
stored yet.

### The gateway delay is high

Alert: `GatewayDelayHigh`.

The gateway delay is the time the gateway adds to a call, without the provider's time.
Targets: 2 ms in proxy mode and 8 ms in managed mode at the 99th percentile.

1. Grafana, "Gateway internals": which mode, and is the time spent before the provider
   call (stage `pre`) or after it (stage `post`)?
2. `docker stats pacod-gw-core-1`: is the core at its CPU limit? Then raise the limit in
   the compose file or add instances.
3. Is audit mode switched on for a busy workspace? It holds complete bodies in memory.
4. Connection time and retry time are shown separately. They depend on the provider and
   the network and are not part of the delay.

### A configuration change does not arrive

Alert: `GatewayConfigurationLag`.

Every instance polls the configuration version in PostgreSQL about once a second.

1. Console, System: which instance is behind?
2. `dc logs --tail 50 <that instance>`: it cannot reach PostgreSQL, or it failed to build
   a snapshot from the new configuration. It keeps serving on the old one.
3. `dc ps postgres`. If PostgreSQL is down, traffic continues on the last snapshot and the
   management API cannot write.
4. Restarting the instance makes it load the current configuration from scratch.

### PostgreSQL, ClickHouse or the disk

- PostgreSQL down: requests are served; limits are counted in memory and written when it
  returns; the console cannot save changes.
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

Console login: `admin@example.com`, password `pacod-gw-dev`. To send requests to the mock
provider, add a provider in the console with the base URL `http://gw-mock:9000`.

The ports are bound to `127.0.0.1`. Never use the override file on a machine that others
can reach. To run a binary directly instead of its container, see `.env.example` in the
repository root.

Stop it with `docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.dev.yml down`.
Add `-v` to delete the data as well.
