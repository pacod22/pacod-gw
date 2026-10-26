# pacod-id runbook

This document is for the person who installs and runs pacod-id: on its own with Docker
Compose, next to a product on the same private network, or on Kubernetes with the Helm chart.
It assumes Docker, PostgreSQL and Kubernetes, and no knowledge of the code.

**State of this document.** On 2026-10-12, on one machine (Docker Desktop on an Apple-silicon
Mac, as in [measurements.md](measurements.md)): the image was built from a clean copy of the
repository; the development stack (section 6) started with every service healthy; pacod-id
connected to PostgreSQL as its own role, which is not a superuser, and created its schema;
the confirmation mail of a sign-up arrived in mailpit; `make measure` passed its targets; and
`make chart-check` (helm lint and kubeconform) passed.

Not exercised: an installation of the Helm chart on a cluster, a real SMTP server, sign-in
with the real Google, an upgrade from one release to the next, and backup and restore. The
operations commands of sections 10 to 12 (`keys`, `rewrap`, `clients secret`) are covered by
the repository's tests but were not run against the stack. Where a step was not exercised,
the text says so, and what it says was read from the code and the deployment files.

Contents:

1. [What runs](#1-what-runs)
2. [Ports](#2-ports)
3. [Configuration](#3-configuration)
4. [Secrets and environment](#4-secrets-and-environment)
5. [Database](#5-database)
6. [Installing with Docker Compose](#6-installing-with-docker-compose)
7. [Installing with Helm](#7-installing-with-helm)
8. [Behind a reverse proxy](#8-behind-a-reverse-proxy)
9. [The first administrator](#9-the-first-administrator)
10. [Signing keys](#10-signing-keys)
11. [Rotating the key-encryption key](#11-rotating-the-key-encryption-key)
12. [Client secrets](#12-client-secrets)
13. [Mail](#13-mail)
14. [Sessions and sign-out](#14-sessions-and-sign-out)
15. [Health, metrics and logs](#15-health-metrics-and-logs)
16. [Backup and restore](#16-backup-and-restore)
17. [Upgrades](#17-upgrades)
18. [Troubleshooting](#18-troubleshooting)

All commands are run from the repository root. `dc` stands for the development stack's
compose command, the same one the Makefile uses:

```sh
dc() { docker compose -f deploy/compose/docker-compose.yml -f deploy/compose/docker-compose.standalone.yml \
  --env-file deploy/compose/.env "$@"; }
```

The image has no shell. The service's own commands (`pacod-id keys list` and the others in
sections 5, 10, 11 and 12) are run inside a running container, where the environment they
need is already set (section 4, "Commands need the whole environment"):

```sh
dc exec pacod-id /usr/local/bin/pacod-id keys list                             # Compose
kubectl -n pacod-id exec deploy/pacod-id -- /usr/local/bin/pacod-id keys list  # Kubernetes
```

## 1. What runs

pacod-id is an OpenID Connect provider for your own products. People sign in on its hosted
pages; products receive tokens, follow a feed of role bindings, and manage accounts through its
internal API. It is one Rust binary, `pacod-id`; `pacod-id serve`, the default command, runs
the service. A product can ship pacod-id with itself: the Dockerfile builds from this
repository or from a product's vendored copy of it.

| Part | What it is | Keeps data in |
|---|---|---|
| `pacod-id` | The service: three HTTP listeners and background work. Any number of instances can share one database. | Nothing of its own. |
| PostgreSQL | Accounts, password hashes, sessions, grants, signing keys (sealed), client secrets (as HMACs), role bindings, the feed, the audit log, the mail and logout outboxes, throttle counters. | Its database, `pacod_id` in the examples. |
| SMTP server | Sends the account mails (section 13). Optional. | – |
| The hosted pages | The sign-in, account and admin pages: a static SvelteKit build that pacod-id reads from `PID_UI_DIR` once at start and serves itself. | The image, `/usr/share/pacod-id/ui`. |

The three listeners:

| Listener | Default address | Serves |
|---|---|---|
| public | `0.0.0.0:8070` | `/.well-known/openid-configuration`, `/oidc/authorize`, `/oidc/token`, `/oidc/jwks`, `/oidc/userinfo`, `/oidc/revoke`, `/oidc/logout`, `/federation/google/callback`, `/ui-api/v1/*` (the pages' API, including that of the admin pages), and the hosted pages on every other path. |
| internal | `0.0.0.0:8071` | For products' servers: `/oidc/token`, `/oidc/jwks`, `/oidc/userinfo`, `/oidc/revoke`, `/internal/v1/*` (catalogues, scopes, organisations, the feed, introspection, session revocation, deletion confirmations) and `/admin/v1/*`. |
| metrics | `0.0.0.0:9170` | `/healthz`, `/readyz`, `/metrics`. |

The public listener answers `/internal/*` and `/admin/v1/*` with 404; the internal listener
serves no pages. Every answer carries `Cache-Control: no-store` unless it sets its own (the
JWKS, the discovery document and the pages' static files do).

**The realm is found from the request.** A realm is one issuer host with its own brand, cookie,
languages and policies (section 3). On the public listener, the browser endpoints and the pages
pick the realm whose `issuer` has the request's `Host` header (host and port). `/oidc/token`
and `/oidc/revoke` pick it from the authenticated client instead, so a product's server can
call them on the internal port and still receive tokens whose `iss` is the public URL.

pacod-id connects out to PostgreSQL, to the SMTP server, to Google's discovery, token and key
endpoints for realms with a `google` block, and to the `backchannel_logout_uri` of clients
(section 14).

Background work:

| What | Runs on | How often |
|---|---|---|
| Send due mails and back-channel logouts | Every instance. Rows are claimed with `FOR UPDATE SKIP LOCKED`, so none goes out twice. | Every 3 s, up to 20 of each per pass. |
| Heartbeat: instance id, key-encryption key id and its check value (section 11) | Every instance. | Every 15 s. |
| Reload the signing keys | Every instance. | Every 30 s. |
| Reload the products' catalogues | Every instance. | Every 10 s. |
| Maintenance: key rotation (section 10) and removal of old rows | One instance, the holder of the lease `maintenance` in the table `jobs` (3 minutes, renewed each pass). | Every 60 s. |

The maintenance removes audit log rows after 400 days, feed changes after 7 days, sent mails
after a day and unsent ones after 7 days, delivered logouts after a day, expired sign-in flows
and codes an hour after they expire, ended sessions and grants a day after they end, throttle
counters after 24 hours, and instance rows not seen for 24 hours. It also blanks the sealed
successor of a rotated refresh token once its 30 seconds are over (section 4).

On SIGTERM an instance reports not ready, ends open long polls of the feed, finishes the
requests in flight, gives up the maintenance lease and removes its instance row.

## 2. Ports

| Port | Variable | Serves | May be reached by |
|---|---|---|---|
| 8070 | `PID_PUBLIC_LISTEN` | Public listener | The internet, **only** through a TLS reverse proxy or an Ingress, on the realms' hosts. |
| 8071 | `PID_INTERNAL_LISTEN` | Internal listener | The products' servers (backends and BFFs) on a private network. Never through the proxy. |
| 9170 | `PID_METRICS_LISTEN` | Metrics and probes | Prometheus and health checks. |

The variables take `address:port`, for example `0.0.0.0:8070`, or a host name with a port,
for example `pacod-id-internal:8071`. A host name is resolved when the listener binds, and the
listener takes the first address that can be bound. In Docker, a name that the container has on
one network alone binds the listener to that network: the pacod stacks bind 8071 to
`pacod-id-internal`, the alias pacod-id has on `pacod-admin-net` only (section 6). The image's
Docker health check calls `http://127.0.0.1:9170/readyz`; if you move the metrics listener,
change the health check as well.

The development stack (section 6) publishes everything on `127.0.0.1` only:

| Host port | Container port | What |
|---|---|---|
| 26070 | `pacod-id` 8070 | Public; the realm's issuer is `http://127.0.0.1:26070`. |
| 26071 | `pacod-id` 8071 | Internal. |
| 26072 | `pacod-id` 9170 | Metrics and probes. |
| 26026 | `mailpit` 8025 | The mail sink's web page. |
| 26080 | `mock-idp` 7080 | The stand-in for Google, as the browser sees it. |

PostgreSQL is not published.

## 3. Configuration

The realms and clients are a TOML file. `PID_CONFIG` (or `--config`) names it; the default is
`config/pacod-id.toml` relative to the working directory, and the image sets
`/etc/pacod-id/pacod-id.toml`. The file is read once at start: after a change, restart every
instance. It holds no secrets; a client's secret comes from the variable its `secret_env`
names. Unknown fields are refused. `config/pacod-id.example.toml` is a commented example.

`${NAME}` in the file is replaced by the environment variable `NAME` before the file is read,
so that one file serves installations that differ in their host names, for example
`issuer = "https://${ID_HOST}"`. A variable that is not set or is empty stops the start with
its name; `$${` stands for a literal `${`. Lines that are comments as a whole are left as they
are (a `${…}` after a value on the same line is not). Values go in as they are: a value holding
a quotation mark or a line break changes the file's meaning.

### Realms

One `[[realm]]` per issuer host.

| Field | Default | Meaning and rules |
|---|---|---|
| `name` | required | Lowercase letters, digits, `-` and `_`, starting with a letter. |
| `issuer` | required | The public URL people use, `https://host[:port]`, without path or trailing slash. It is the `iss` of every token of the realm. No two realms may share a host. `http` is accepted for development only (see below). |
| `brand` | required | `gateway`, `cms`, `kb`, `shop` or `bazaar`: the pages' theme and the mails' layout. |
| `locales` | `["en", "bg", "de", "ru"]` | Languages of the pages. Each must be one of those four. |
| `default_locale` | `"en"` | Must be one of `locales`. |
| `mail_locales` | `["bg", "en"]` | Every mail carries all of these, in this order. |
| `self_signup` | `false` | Whether anyone may create an account. A client may override it. |
| `password_min_length` | `12` | 8 to 128. |
| `role_holder_password_min_length` | `12` | At least `password_min_length`. Applies to accounts that hold any role. |
| `session_hours` | `12` | A sign-in without "keep me signed in". Must be positive. |
| `remember_me_days` | `7` | A sign-in with "keep me signed in". Must be positive. |
| `verification_link_hours` | `24` | Lifetime of an address confirmation link. |
| `reset_link_minutes` | `60` | Lifetime of a password reset link. |
| `invitation_days` | `7` | Lifetime of an invitation. |
| `accounts_managed_by` | `[]` | Roles whose holders create, disable and reset accounts in this realm, for example `["gw.platform_admin"]`. |
| `terms_url`, `privacy_url` | none | When `terms_url` is set, a sign-up must accept the terms. |
| `terms_version` | `"1"` | Recorded with the acceptance. |
| `require_verified_email` | `true` | An account confirms its address before it signs in. Only applies when SMTP is configured (section 13). |
| `google` | none | Sign-in with Google for this realm (below). Without it, the button does not show. |

With an `https` issuer the cookies are `Secure` and carry the `__Host-` prefix. With an `http`
issuer they are neither; use `http` only on `127.0.0.1` or in tests.

### Clients

One `[[client]]` per application. There is no dynamic registration.

| Kind | Has a secret | Grants by default | Must have |
|---|---|---|---|
| `confidential` | Yes | `authorization_code`, `refresh_token` | `audience`, at least one redirect URI |
| `public` | No; PKCE alone protects its codes | `authorization_code`, `refresh_token` | `audience`, at least one redirect URI |
| `service` | Yes | `client_credentials`, `token_exchange` | `audience` while it has `token_exchange`, which it has by default; `product` to use the internal API |

| Field | Default | Meaning and rules |
|---|---|---|
| `id` | required | The `client_id`. Lowercase letters, digits, `-` and `_`. |
| `realm` | required | A configured realm. |
| `kind` | required | `confidential`, `public` or `service`. |
| `secret_env` | none | The name of the variable holding the secret (section 12). Refused for public clients. Without it, the secret set with `pacod-id clients secret` counts. |
| `redirect_uris` | `[]` | Exact URIs, no wildcards. Absolute `http` or `https`, without fragment. |
| `post_logout_redirect_uris` | `[]` | Exact URIs a sign-out may return to. |
| `backchannel_logout_uri` | none | Where pacod-id posts logout tokens (section 14). May be an internal address, such as `http://ui:3000/auth/backchannel-logout`. |
| `initiate_login_uri` | none | Where the product starts a sign-in; the pages send people there after they accept an invitation. |
| `audience` | none | Required for confidential and public clients: the product the access tokens are for (`aud`), for example `pacod-gw`. Required for service clients with `token_exchange`: the audience of the people's tokens it may exchange, its product's own (the gateway's core: `pacod-gw`); tokens for any other audience are refused. |
| `product` | none | For service clients: the product it speaks for (its catalogue, its scopes, its feed), for example `gw`. A service client without one only exchanges tokens and introspects, for example a product's tester taking delegated grants: the internal API refuses it, and it may end only sessions it had a grant in. |
| `grants` | by kind | Any of `authorization_code`, `refresh_token`, `client_credentials`, `token_exchange`. Service clients may not sign people in; other clients may not use the service grants. |
| `access_ttl_seconds` | `300` | 120 to 900. |
| `id_token_alg`, `access_token_alg` | `"RS256"` | `RS256` or `ES256`. An ES256 signing key is made at start once a client asks for ES256. |
| `self_signup` | the realm's | Overrides the realm's sign-up policy for this client. |
| `roles_claim` | none | For one client that needs it: a `roles` claim in the id_token and userinfo, `{ values = { admin = "shop.webui.admin" }, default = "user" }`, each value whose permission the account holds, else `default`. |
| `refresh_idle_days`, `refresh_absolute_days` | `7`, `365` | For refresh tokens not bound to a browser session (a public client, such as a mobile app): ended after this many days unused, and at most after this many. |

### Google

The `google` block of a realm is that realm's own OAuth client at Google:

```toml
google = { client_id = "1234-abc.apps.googleusercontent.com", secret_env = "PID_GOOGLE_GW" }
```

| Field | Default | Meaning |
|---|---|---|
| `client_id` | required | The OAuth client id from Google. |
| `secret_env` | required | The variable holding its client secret. |
| `issuer` | `https://accounts.google.com` | The provider's issuer. |
| `server_url` | none | Where pacod-id itself reaches the provider, when that differs from the address browsers use. Only the development stack's mock needs it. |

Register `<issuer>/federation/google/callback` as the redirect URI of the Google client, one
per realm. pacod-id reads Google's discovery document when the first person chooses Google (and
then once an hour), and Google's keys when an unknown key id shows up, at most once a minute.
It needs outbound HTTPS to Google for that. Google sign-in was not exercised against Google
itself, only against the mock.

### Checking the file

```sh
docker run --rm -v "$PWD/pacod-id.toml:/etc/pacod-id/pacod-id.toml:ro" pacod-id:dev check-config
```

It prints `<file>: <n> realms, <m> clients`, or the first mistake with its place, for example
`client pacod-gw-console: an audience (the product, e.g. pacod-gw) is required`, and exits
non-zero. `check-config` needs no database, and no variable but those the file names with
`${…}`. It does not check that the
secrets the file names are set, that DNS or an Ingress points to the issuers, or that Google
is reachable.

## 4. Secrets and environment

### Three ways to give a secret

Each variable marked "file" below can be given in one of three ways:

1. The variable itself: `PID_KEK=...`.
2. `NAME_FILE`, the path of a file holding it: `PID_KEK_FILE=/run/secrets/pid_kek`.
3. A file named after the variable in the directory `PID_SECRETS_DIR`:
   `PID_SECRETS_DIR=/var/run/secrets/pacod-id` and the file
   `/var/run/secrets/pacod-id/PID_KEK`.

Setting both `NAME` and `NAME_FILE` is refused at start ("PID_KEK and PID_KEK_FILE are both
set; set one of them"). `NAME` wins over `PID_SECRETS_DIR`; a file missing from the directory
leaves the variable unset. Leading and trailing white space of a file is ignored, and an empty
value counts as unset. Besides the `PID_*` secrets below, every `secret_env` of a client and of
a `google` block can be given the same three ways. Other variables are read from the
environment only. Error messages name the variable and the file, never the value.

### Variables

| Variable | Required | Default | What it is |
|---|---|---|---|
| `PID_CONFIG` | no | `config/pacod-id.toml` (image: `/etc/pacod-id/pacod-id.toml`) | The configuration file (section 3). |
| `PID_DATABASE_URL` (file) | yes | – | `postgres://pacod_id:<password>@<host>:5432/pacod_id?sslmode=require`. |
| `PID_KEK` (file) | yes | – | The key-encryption key: base64 of exactly 32 bytes. |
| `PID_KEK_ID` | no | `default` | The id of `PID_KEK`, stored with everything sealed under it (section 11). |
| `PID_KEK_OLD` (file) | no | – | Earlier key-encryption keys during a rotation: `id=base64key,id=base64key`. |
| `PID_TOKEN_PEPPER` (file) | yes | – | The HMAC key for client secrets, service tokens and known devices. At least 16 bytes. |
| `PID_LEGACY_PEPPERS` (file) | no | – | Peppers of products whose tokens were imported: `gw:<the gateway's PGW_KEY_PEPPER>`. Only `gw` (tokens `pgwa_...`) is recognised. |
| `PID_PUBLIC_LISTEN` | no | `0.0.0.0:8070` | Public listener. |
| `PID_INTERNAL_LISTEN` | no | `0.0.0.0:8071` | Internal listener. |
| `PID_METRICS_LISTEN` | no | `0.0.0.0:9170` | Metrics and probes. |
| `PID_TRUSTED_PROXIES` | behind a proxy | none | Proxies whose `X-Forwarded-For` is believed: addresses, CIDR ranges and the word `private_ranges`, comma-separated (section 8). |
| `PID_ARGON2_M_KIB` | no | `19456` | Memory of one password hash in KiB (Argon2id, 2 passes). Values below the default count as the default. |
| `PID_UI_DIR` | no | `ui/build` (image: `/usr/share/pacod-id/ui`) | The hosted pages' build. |
| `PID_LOG` | no | `info,sqlx=warn` | Log level: `debug`, `info`, `warn` or `error` (any case; `warning` is `warn`), or a filter in the syntax of `tracing`'s `EnvFilter` (`info,sqlx=warn`). Any other value gives the default, and the log's first line says so (section 15, "Logs"). |
| `PID_LOG_FORMAT` | no | `json` | `json` or `text`, any case; any other value gives JSON, one object per line. |
| `SERVICE_NAME` | no | `pacod-id` | The `service` of every log line: the compose service name. |
| `PID_SMTP_HOST` | no | – | The SMTP server. Without it no mail is sent (section 13). |
| `PID_SMTP_PORT` | no | 587, 465 or 25 by `PID_SMTP_TLS` | The SMTP port. |
| `PID_SMTP_TLS` | no | `starttls` | `starttls`, `tls` or `none`; with `PID_SMTP_HOST`, any other value stops the start. |
| `PID_SMTP_USERNAME` | no | – | With `PID_SMTP_HOST`, needs `PID_SMTP_PASSWORD`, or the start fails. |
| `PID_SMTP_PASSWORD` (file) | no | – | Used only together with the user name; alone it is ignored. |
| `PID_EMAIL_FROM_ADDRESS` | with `PID_SMTP_HOST` | – | The sender address. |
| `PID_EMAIL_FROM_NAME` | no | `pacod-id` | The sender name. |
| `PID_BOOTSTRAP_ADMIN_EMAIL` | no | – | The first administrator (section 9). |
| `PID_BOOTSTRAP_ADMIN_PASSWORD` (file) | to create that account | – | Its password. Refused without `PID_BOOTSTRAP_ADMIN_EMAIL`. |
| `PID_BOOTSTRAP_ADMIN_ROLES` | no | `id.admin` | Its roles, comma-separated. |
| `PID_BOOTSTRAP_ADMIN_REALM` | with more than one realm | the only realm | The realm its account is created in. |
| `PID_SECRETS_DIR` | no | – | A directory with one file per secret, named after the variable. |
| each client's `secret_env` (file) | yes, for each client that names one | – | The client's secret, at least 32 characters (section 12). |
| each `google.secret_env` (file) | yes, for each realm with Google | – | The Google client secret. |

The development stack's mock Google reads `PID_MOCK_ISSUER`, `PID_MOCK_LISTEN` and
`PID_MOCK_CLIENTS`; it is never deployed.

### What each secret protects

| Secret | Generate with | Protects | If it is lost or changed |
|---|---|---|---|
| `PID_KEK` | `openssl rand -base64 32` | The private signing keys, the parameters of queued mails (they hold one-time links), and for 30 seconds the successor of a rotated refresh token (the maintenance blanks it afterwards, section 1). All are sealed under it in the database. | **The service does not start** ("signing key ...: cannot decrypt (is PID_KEK right?)"). To change it, rotate it (section 11). If it is lost, see below. |
| `PID_TOKEN_PEPPER` | `openssl rand -base64 32` | HMACs of client secrets, of service tokens (`pidt_...`) and of known-device cookies. | Secrets set with `pacod-id clients secret` and every service token stop working and must be issued again. Secrets from `secret_env` keep working: their HMAC is written again at every start. Every browser counts as new: the lockout exemption of known devices starts over, and people who signed in elsewhere before get a "new sign-in" mail at their next sign-in. Refresh tokens, sessions and links are stored as SHA-256 and are not affected. |
| `PID_LEGACY_PEPPERS` | the product's own pepper | Imported admin tokens of the gateway (`pgwa_...`). | Those tokens stop working. |
| Database password | `openssl rand -hex 24` | The role `pacod_id`. Hex, because it goes into a URL. | Change it in PostgreSQL and in `PID_DATABASE_URL`, then restart. |
| Client secrets (`secret_env`) | `openssl rand -hex 32` | A product's calls to pacod-id. | The product's calls fail until both sides have the same value (section 12). |
| `PID_BOOTSTRAP_ADMIN_PASSWORD` | `openssl rand -base64 18` | Used once, to create the first administrator. | Nothing, once the account exists. |
| `PID_SMTP_PASSWORD` | from the mail provider | The SMTP account. | Mails are retried for about eight hours, then dropped (section 13). |
| Google client secret | from Google | Sign-in with Google. | Google sign-in fails; passwords keep working. |

Keep `PID_KEK` and `PID_TOKEN_PEPPER` outside the database backups (section 16). A database
dump together with the key-encryption key is enough to sign tokens.

**If `PID_KEK` is lost.** Not exercised; read from the code. The sealed signing keys cannot be
read, so the service does not start. Deleting the rows of the table `signing_keys` lets it
start: it then makes a new RS256 key (and an ES256 key if a client uses ES256). Every id_token
and access token issued before stops verifying (they live 15 minutes at most), queued mails
cannot be sent, and products must read the JWKS again (section 10). Accounts, passwords,
sessions and refresh tokens are not sealed and keep working.

```sql
DELETE FROM signing_keys;   -- only when PID_KEK is lost for good
```

### Commands need the whole environment

Every command except `check-config` (`serve`, `migrate`, `keys`, `clients secret`, `rewrap`)
reads the whole environment first and refuses to run without the database URL, `PID_KEK`,
`PID_TOKEN_PEPPER`, and every client and Google secret the configuration names. Run them in a
running container (see the top of this document) or give a separate container or Kubernetes
Job the same environment and Secret as the service.

## 5. Database

pacod-id needs a PostgreSQL database of its own and a role that owns it and is not a
superuser. The migrations create no extensions, so ownership of the database is enough. The
development stack makes both at every start (`platform/postgres/id-db.sh`, idempotent); on
your own server, as a superuser:

```sql
CREATE ROLE pacod_id LOGIN PASSWORD '<password>' NOSUPERUSER NOCREATEDB NOCREATEROLE;
CREATE DATABASE pacod_id OWNER pacod_id;
REVOKE ALL ON DATABASE pacod_id FROM PUBLIC;
```

The development stack uses PostgreSQL 18; no other version was tried.

**Migrations run at every start.** `pacod-id serve` brings the schema up to date before it
serves. sqlx, the migration library, takes a PostgreSQL advisory lock first, so instances that
start together wait for each other and the migrations run once. To run them separately, for
example before a rolling upgrade:

```sh
dc run --rm pacod-id migrate              # Compose; prints "the database is up to date"
kubectl -n pacod-id exec deploy/pacod-id -- /usr/local/bin/pacod-id migrate
```

`pacod-id keys list`, `keys rotate` and `clients secret` also migrate before they run; a newer
image's command run against your database migrates it. `rewrap` does not.

Migrations only add; there are no down migrations (section 17). The schema is in
`crates/pacod-id/migrations/`: `0001_init.sql` and the migrations after it. A migration is never
changed once a database may have applied it: a change is a new migration.

**Connections.** Each instance opens a pool of up to 16 connections (`application_name`
`pacod-id`), plus one that listens on the channel `pid_authz` for the feed. Connect directly or
through a pooler in session mode; a pooler in transaction mode breaks `LISTEN`. When the
listening connection is lost, the instance logs it and polls once a second until it is back.
Set `sslmode` in `PID_DATABASE_URL` as your server requires.

## 6. Installing with Docker Compose

The repository has a development stack, `deploy/compose/`. There is no production compose file:
use the stack as the starting point and change what the end of this section lists.

### The development stack

```sh
make compose-up      # creates the networks pacod-net and pacod-admin-net, deploy/compose/init-env.sh, then: dc up -d --build --wait
```

The stack follows the shared pacod convention (`pacod-demo/docs/NETWORK.md`): every container
joins the external network `pacod-net` and has a name that is unique across all pacod repos.
`pacod-id` also joins `pacod-admin-net`, an internal network (no route out) for the listeners
that only servers reach, under the alias `pacod-id-internal`; products' servers call
`http://pacod-id-internal:8071` there. Here the internal listener stays on every interface,
because 26071 publishes it to the host and a published port never reaches an internal network.
The full stack publishes nothing and binds it to the alias, so that the rest of `pacod-net`
cannot reach it. `make network` creates both networks and stops when `pacod-admin-net` exists
without being internal; with plain `docker compose`, create them first:

```sh
docker network create pacod-net
docker network create --internal pacod-admin-net
```

`deploy/compose/docker-compose.yml` holds pacod-id alone; `docker-compose.standalone.yml` holds
the development copies of the shared components it uses (`pacod-postgres`, `pacod-db-init`,
`pacod-mailpit`) and the mock Google. The full pacod stack (`pacod-demo`) loads only the first
file and runs one shared PostgreSQL, mail sink and edge for every product. `platform/` holds
pacod-id's share of that shared configuration: its database script
(`platform/postgres/id-db.sh`), its route on the shared Caddy (`platform/caddy/id.caddy`) and
its Prometheus job (`platform/prometheus/id.yml`).

`init-env.sh` writes `deploy/compose/.env` once, readable by its owner only, with random values
for the PostgreSQL superuser and the role `pacod_id`, `PID_KEK`, `PID_TOKEN_PEPPER`, the two
clients' secrets, the mock Google's secret and the first administrator's password. It never
prints them and leaves an existing file alone. Then compose builds the images `pacod-id:dev`
and `pacod-id-mock-idp:dev` and starts, as the project `pid-dev`:

| Service | What it is |
|---|---|
| `pacod-postgres` | PostgreSQL 18.6. Volume `pacod-postgres-data`. |
| `pacod-db-init` | Runs `platform/postgres/id-db.sh` at every start: the role `pacod_id` and the database `pacod_id`, created when missing. |
| `pacod-id` | The service, with a read-only root file system and a small `/tmp`. Connects as `pacod_id`. Health check `/readyz`. |
| `pacod-mailpit` | A mail sink: every mail the stack sends ends here, none leaves the machine. Web page on `127.0.0.1:26026`. |
| `id-mock-idp` | Stands in for Google: issuer `http://127.0.0.1:26080` for the browser; pacod-id calls it at `http://id-mock-idp:7080` (`server_url`). |

The configuration is `deploy/compose/pacod-id.toml`: one realm `shop` at
`http://127.0.0.1:26070`, sign-up open, with the mock Google; a confidential client `dev-app`
whose redirect URI `http://127.0.0.1:26099/callback` nobody listens on (the address bar shows
the code); and a service client `dev-service` for the product `dev`, which may exchange tokens
issued to `dev-app` (its `audience`). The first administrator is `admin@pacod-id.dev.test`
with the role `id.admin`; its password is `PID_BOOTSTRAP_ADMIN_PASSWORD` in
`deploy/compose/.env`.

Check it:

```sh
dc ps                                                     # every service healthy
curl -s http://127.0.0.1:26072/readyz                     # ready
curl -s http://127.0.0.1:26070/.well-known/openid-configuration
dc logs -f pacod-id
```

Open `http://127.0.0.1:26070/signup` to make an account; the confirmation mail shows on
`http://127.0.0.1:26026`. `http://127.0.0.1:26070/admin` opens the admin pages after sign-in.

```sh
make measure         # latency of the token endpoint and the feed against the stack
make compose-down    # stop; the database volume stays
dc down -v           # stop and delete the database
```

### For production

1. **TLS in front of port 8070.** Put a reverse proxy (Caddy, nginx, Traefik) in front and
   publish only the proxy. Do not publish 8071 and 9170 on a public address; products reach
   8071 over a private Docker network. Bind 8071 to that network alone
   (`PID_INTERNAL_LISTEN=pacod-id-internal:8071`, the alias on `pacod-admin-net`), not to
   `0.0.0.0`: on `0.0.0.0` every container that shares any network with pacod-id reaches it.
   Section 8 lists what the proxy must do.
2. **Issuers.** Every realm's `issuer` is `https://<its host>`, with DNS pointing at the proxy.
3. **`PID_TRUSTED_PROXIES`** set to the proxy's address (section 8).
4. **Real SMTP**: `PID_SMTP_HOST`, `PID_SMTP_PORT`, `PID_SMTP_TLS` (`starttls` or `tls`),
   `PID_SMTP_USERNAME`, `PID_SMTP_PASSWORD`, `PID_EMAIL_FROM_ADDRESS`, `PID_EMAIL_FROM_NAME`.
   Remove mailpit.
5. **Google**: remove `mock-idp` and `server_url`; register a Google client per realm.
6. **Secrets** in files rather than in the environment: Docker secrets with
   `PID_SECRETS_DIR=/run/secrets` and each secret named after its variable (not exercised).
   Keep the `.env` and secret files out of version control and out of the database backup.
7. **PostgreSQL** on a server you back up (section 16), with the role of section 5.
8. **The image**: build a tagged image (`make image VERSION=<version>`) or pull a released
   one, and pin it by digest.
9. **The first administrator**: remove `PID_BOOTSTRAP_ADMIN_*` once it has signed in
   (section 9).
10. Keep `PID_LOG_FORMAT` at `json` (the default) for a log collector.

### Make targets

| Target | What it does |
|---|---|
| `make image` | Builds `pacod-id:$(VERSION)` (default `dev`) from `deploy/docker/pacod-id.Dockerfile`. |
| `make image-mock` | Builds the mock Google image. |
| `make compose-up`, `make compose-down` | The development stack, as above. |
| `make measure` | Token endpoint p99 under 50 ms and feed p99 under 1 s, against the stack ([measurements.md](measurements.md)). |
| `make chart-check` | `helm lint` and kubeconform over the chart, with secrets as files, as variables and from an existing Secret; then, when the image `pacod-id:$(VERSION)` exists (`make image`), `pacod-id check-config` on the configuration the chart renders from `ci/test-values.yaml` (otherwise it says that this check was skipped). Needs Docker only. |
| `make test-services` | The tests' PostgreSQL on `127.0.0.1:25434`. |
| `make e2e` | The hosted pages in a real browser, with three realms, mailpit and the mock Google. |

The image is built from the repository root (or from a vendored copy's root). It runs as the
user 65532 on a distroless base without a shell; the binaries are linked statically against
musl.

## 7. Installing with Helm

The chart is `deploy/helm/pacod-id`. **It was checked with `helm lint` and kubeconform
(`make chart-check`) but has not been installed on a cluster yet.** Everything in this section
is read from the templates.

```sh
helm upgrade --install pacod-id deploy/helm/pacod-id \
  --namespace pacod-id --create-namespace -f pacod-id-values.yaml
```

It creates a Deployment, a ConfigMap with the configuration file, a Secret (unless
`secrets.existingSecret` is set), a ServiceAccount without a mounted token, two Services, an
Ingress, a NetworkPolicy, a PodDisruptionBudget, and optionally a ServiceMonitor. PostgreSQL is
not part of it.

| Service | Ports | For |
|---|---|---|
| `<release>` (here `pacod-id`) | 8070 | The Ingress. |
| `<release>-internal` | 8071, 9170 | Products: `http://pacod-id-internal.pacod-id.svc:8071`; and the scrapers. |

### Values that matter

| Value | Default | Meaning |
|---|---|---|
| `image.registry`, `image.repository`, `image.tag` | `""`, `pacod-id`, the chart's appVersion | Prefer a digest-pinned tag. |
| `replicas` | `2` | Any number is safe; two keep sign-in up when a node goes away. |
| `config` | required | The TOML of section 3, as a string. |
| `secrets.existingSecret` | `""` | Use your own Secret instead of the chart's (below). |
| `secrets.mount` | `true` | `true`: the Secret is mounted at `/var/run/secrets/pacod-id` and read through `PID_SECRETS_DIR`, so the secrets are not in the process environment. `false`: every key becomes a variable. |
| `secrets.databaseUrl`, `kek`, `tokenPepper` | required without `existingSecret` | `PID_DATABASE_URL`, `PID_KEK`, `PID_TOKEN_PEPPER`. |
| `secrets.kekOld`, `legacyPeppers`, `smtpPassword`, `bootstrapAdminPassword` | `""` | The matching `PID_*` variables. |
| `secrets.extra` | `{}` | Client and Google secrets, by the variable names the configuration gives them, for example `PGW_UI_CLIENT_SECRET: ...`. |
| `kekId` | `default` | `PID_KEK_ID` (section 11). |
| `email.smtpHost`, `smtpPort`, `smtpTls`, `smtpUsername`, `fromAddress`, `fromName` | `""`, `""`, `starttls`, ... | Mail. `fromAddress` is required with `smtpHost`. |
| `bootstrapAdmin.email`, `realm`, `roles` | `""`, `""`, `id.admin` | Section 9. `realm` is needed only with more than one realm. |
| `trustedProxies` | `""` | `PID_TRUSTED_PROXIES`: the Ingress controller's pod addresses (section 8). Required while `ingress.enabled` (the default is `true`): the chart fails to render without it. |
| `logFormat`, `logLevel` | `json`, `info,sqlx=warn` | `PID_LOG_FORMAT`, `PID_LOG`. |
| `extraEnv` | `[]` | More variables, as in a pod spec. |
| `resources` | 100m CPU and 128Mi requested, 512Mi memory limit | See "Memory" below. |
| `podDisruptionBudget.enabled`, `minAvailable` | `true`, `1` | With `replicas: 1` this blocks node drains; raise replicas or turn it off. |
| `ingress.className`, `annotations`, `hosts` | `""`, `{}`, `[]` | One entry per realm issuer: `{ host: id.example.com, tls: { secretName: ... } }`. Required while `ingress.enabled`. |
| `networkPolicy.publicFrom` | `[]` | Who may reach 8070. Empty: anyone. |
| `networkPolicy.internalFrom` | `[]` | Who may reach 8071. **Set it**: empty means nothing reaches the internal port. The chart only warns about it in its notes. |
| `networkPolicy.metricsFrom` | `[]` | Who may reach 9170. Empty means no scraper can; the kubelet's probes are not subject to policies. Set it when you scrape. |
| `serviceMonitor.enabled` | `false` | A Prometheus Operator ServiceMonitor for `/metrics`. Needs `metricsFrom`. |

The NetworkPolicy covers incoming traffic only. Outgoing traffic (PostgreSQL, SMTP, Google,
back-channel logout URIs) is not restricted by the chart.

### Secrets

Without `existingSecret` the chart writes a Secret from `secrets.*`. With it, the Secret you
name must have the keys `PID_DATABASE_URL`, `PID_KEK` and `PID_TOKEN_PEPPER`, and may have
`PID_KEK_OLD`, `PID_LEGACY_PEPPERS`, `PID_SMTP_PASSWORD`, `PID_BOOTSTRAP_ADMIN_PASSWORD` and
one key per `secret_env` of the configuration. The key names are the variable names: with
`secrets.mount: true` they become the file names pacod-id looks for.

pacod-id reads its secrets and its configuration only at start. A change to `config` or to the
chart's own Secret changes a checksum on the pods and restarts them. A change to an
`existingSecret` does not:

```sh
kubectl -n pacod-id rollout restart deployment/pacod-id
```

### Example values

Placeholders only:

```yaml
image:
  registry: registry.example.com/platform
  tag: "0.1.0@sha256:<digest>"
config: |
  [[realm]]
  name = "gateway"
  issuer = "https://id.example.com"
  brand = "gateway"
  accounts_managed_by = ["gw.platform_admin"]

  [[client]]
  id = "pacod-gw-console"
  realm = "gateway"
  kind = "confidential"
  secret_env = "PGW_UI_CLIENT_SECRET"
  redirect_uris = ["https://console.example.com/auth/callback"]
  audience = "pacod-gw"
secrets:
  existingSecret: pacod-id-secrets
kekId: "2026-10"
email:
  smtpHost: smtp.example.com
  smtpPort: "587"
  smtpUsername: id@example.com
  fromAddress: id@example.com
  fromName: Sign-in
bootstrapAdmin:
  email: admin@example.com
  realm: gateway
trustedProxies: 10.244.0.0/16           # the cluster's pod range, see section 8
ingress:
  className: nginx
  hosts:
    - host: id.example.com
      tls:
        secretName: id-example-com-tls
networkPolicy:
  publicFrom:
    - namespaceSelector:
        matchLabels: { kubernetes.io/metadata.name: ingress-nginx }
  internalFrom:
    - namespaceSelector:
        matchLabels: { kubernetes.io/metadata.name: pacod-gw }
```

### Pods

- Probes on port 9170: startup `/readyz` every 2 s for up to 3 minutes, readiness `/readyz`
  every 5 s, liveness `/healthz` every 10 s.
- Rolling updates add one pod before removing one (`maxSurge: 1`, `maxUnavailable: 0`), and a
  stopping pod gets 30 seconds.
- User 65532, read-only root file system, all capabilities dropped, `/tmp` an `emptyDir` of
  16Mi.
- **Memory.** A password hash takes `PID_ARGON2_M_KIB` (19 MiB by default). An instance
  computes at most twice as many at once as it sees CPUs, between 2 and 16: up to about
  300 MiB in hashes on a large node at the default cost. If you raise `PID_ARGON2_M_KIB`, raise
  the memory limit with it.

## 8. Behind a reverse proxy

The proxy in front of port 8070 must:

- **Pass the original `Host` header.** The realm is chosen by it, and it must equal the
  issuer's host and port. pacod-id does not read `X-Forwarded-Host` or `X-Forwarded-Proto`; the
  scheme comes from the issuer in the configuration. A wrong host gives 404 "No sign-in service
  is configured at this address."
- **Leave the browser's `Origin` alone.** Every write from the pages must carry
  `Origin: <issuer origin>`; otherwise pacod-id answers 403 with the code `cross_origin`.
- **Set `X-Forwarded-For`** with the client's address, and be named in `PID_TRUSTED_PROXIES`.
- **Keep query strings out of its access log** for the pacod-id hosts, or strip at least
  `token`, `code`, `state`, `id_token_hint` and `login_hint`. Links in mails carry one-time
  tokens in the URL (`/verify-email?token=`, `/reset-password?token=`, `/invite?token=`).
  pacod-id itself writes no access log.

### Why `PID_TRUSTED_PROXIES` matters

Sign-in and sign-up are limited per client address. Without `PID_TRUSTED_PROXIES` every request
seems to come from the proxy, and these limits count all people together:

| Limit | Counted per | Effect when reached |
|---|---|---|
| Failed sign-ins | Client address (IPv4, or IPv6 /64) | After 30 failed attempts in 15 minutes, the next is refused and no sign-in from that address is possible for 15 minutes. **Behind an untrusted proxy, nobody can sign in.** |
| Failed sign-ins | Account, on browsers it has not signed in on before | After 10 failed attempts in 15 minutes, the next locks the account on such browsers for 15 minutes. |
| Failed sign-ins | Account, on a browser it has signed in on before (the `pid_dev` cookie) | After 10 failed attempts in 15 minutes, the next locks it on that browser only; others cannot lock its owner out. |
| Sign-ups | Client address | 10 an hour; the 11th answers 429 "too many sign-ups from this network; try again later". |
| Mail requests (confirmation, reset) | Client address | 20 an hour; further requests are answered as usual but send nothing. |
| Mails of one kind to one address | Address | 3 in 15 minutes; further ones are dropped silently. |

The counters are in PostgreSQL (table `throttle`), so every instance sees the same numbers. A
sign-in attempt is counted before its password is checked, and a correct password takes it
back, so many attempts sent at once get no more tries than the limit. The client address is
also what the audit log and the session list record.

### How the address is read

`PID_TRUSTED_PROXIES` takes addresses (`192.0.2.7`), CIDR ranges (`10.0.0.0/8`, `fd00::/8`)
and the word `private_ranges` (10/8, 172.16/12, 192.168/16, 127/8, fc00::/7, ::1), separated
by commas. When the connection comes from a trusted address, pacod-id reads `X-Forwarded-For`
from the right and takes the first address that is not itself trusted. A header sent by
anyone else is ignored.

Name your proxies as narrowly as you can. A trusted range lets a client inside it choose the
address it is counted under:

- A client that reaches port 8070 from a trusted address without passing the proxy can send
  its own `X-Forwarded-For`. Keep 8070 reachable only from the proxy (on Kubernetes,
  `networkPolicy.publicFrom` set to the Ingress controller's namespace); products' servers can
  use the token endpoint on 8071 instead.
- A client on a private network that goes through a proxy which appends to the header (nginx's
  `$proxy_add_x_forwarded_for` does) produces `<forged>, 10.x.x.x`. With `private_ranges` the
  `10.x.x.x` hop is trusted and skipped, and the forged address counts. Where people sign in
  from private addresses (an intranet), trust only the proxy's own address, or have the proxy
  replace the header instead of appending to it.

## 9. The first administrator

At every start pacod-id looks at `PID_BOOTSTRAP_ADMIN_EMAIL`. When no account has that
address, it creates one, in the realm `PID_BOOTSTRAP_ADMIN_REALM` (needed only with more than
one realm), with the password `PID_BOOTSTRAP_ADMIN_PASSWORD` and the roles
`PID_BOOTSTRAP_ADMIN_ROLES` (default `id.admin`) at the platform. The address counts as
confirmed. When an account with that address exists, it changes nothing and logs "the
bootstrap admin's account exists; PID_BOOTSTRAP_ADMIN_* change nothing". Several instances
starting at once create the account once.

- **Roles** are written `product.role`, comma-separated. `id.admin` may do everything in
  pacod-id and cannot lose its last holder; `id.auditor` reads accounts and the audit log. A
  product's role, such as `gw.platform_admin`, must be one held at the platform. When the
  product has not pushed its catalogue yet, the binding is kept and counts for nothing until
  the catalogue arrives; the log says "the bootstrap admin's role waits for its product's
  catalogue".
- **The password** must meet the realm's minimum for role holders
  (`role_holder_password_min_length`, 12 by default), be at most 128 characters, and not be one
  of the common passwords pacod-id refuses. Otherwise the service does not start:
  "PID_BOOTSTRAP_ADMIN_PASSWORD is refused (password_too_short); role holders need 12
  characters or more".

Then:

1. Open `<issuer>/admin` in a browser and sign in with the address and the password.
2. Change the password on `<issuer>/account/security` if the first one was shared.
3. Remove both `PID_BOOTSTRAP_ADMIN_PASSWORD` and `PID_BOOTSTRAP_ADMIN_EMAIL` (on Helm,
   `bootstrapAdmin.email` and `secrets.bootstrapAdminPassword`) and restart.

Remove both: if the account is ever deleted while the address is still set without a password,
every start fails with "PID_BOOTSTRAP_ADMIN_PASSWORD (or _FILE) is needed to create the
bootstrap admin". In a realm with `self_signup = true`, someone could sign up with the address
before the first start; check the log for "created the bootstrap admin".

## 10. Signing keys

pacod-id signs id_tokens, access tokens and logout tokens with keys it makes itself, kept in
the table `signing_keys` with the private key sealed under `PID_KEK`. One RS256 key (2048
bits) signs at a time. An ES256 key is made at start once a client asks for ES256, and is
then rotated like the RS256 key. The JWKS
(`/oidc/jwks`, on both the public and the internal port) publishes every key that is `next`,
`active` or `retired`, with `Cache-Control: public, max-age=3600`.

**Automatic rotation.** The instance holding the maintenance lease checks every minute:

- 24 hours before the active key has signed for 90 days, it makes the next key and publishes
  it in the JWKS (state `next`).
- At the 90 days, the next key becomes `active` and the old one `retired`.
- A retired key stays in the JWKS for 49 hours (the longest token lifetime, 15 minutes, plus
  two days), then it is deleted.

Every instance reloads the keys every 30 seconds. Nothing has to be done by hand.

```sh
pacod-id keys list
```

prints one line per key: id, algorithm, state, when it activates, and for a retired key when
it goes. Key ids look like `rs256-20261012-<8 characters>`.

**Emergency rotation**, when a private key may have leaked:

```sh
pacod-id keys rotate --revoke <kid>            # --alg ES256 for the ES256 key
```

It makes a new active key at once. The active key of that algorithm is retired (still
published), or deleted when it is the one `--revoke` names. Any key named with `--revoke` is
deleted from the JWKS, so tokens it signed stop verifying. It prints "<kid> signs now; running
instances take it within 30 seconds". An id that does not exist is refused, and nothing is
rotated or deleted: the command exits non-zero with "no key <kid>: see `pacod-id keys list`;
nothing changed". If `keys list` shows a key in state `next`, it will still take over at its
time; if it may be affected too, revoke it with a second `keys rotate --revoke <its kid>`. If
`PID_KEK` may have leaked as well, rotate it first (section 11).

**What clients see.** pacod-id itself stops accepting a revoked key within 30 seconds. Products
cache the JWKS. The Rust and Python client libraries (`pacod-id-client`, `pacod_id`) read it
again when a token names an unknown key, one read at a time and at most every 30 seconds, and
otherwise every hour; that hourly read runs in the background, so the request that triggers it
is still answered with the keys it has. A read gives up after 5 seconds, and a pacod-id that
does not answer never holds up a request whose key is known. A newly active key is therefore
picked up within 30 seconds, but a revoked key stays trusted by a product until its next
hourly read. The hour can be shortened per product: `Verifier::with_refresh_every(client,
every)` in Rust, `JwtVerifier(client, refresh_every=…)` (seconds) in Python. **Restart the
products after revoking a key** if they must refuse its tokens at once. A product's own
service token signed with the revoked key is refused by pacod-id; both libraries then fetch a
new one and retry the call once (in Rust, every call except `deletion_done` and
`revoke_session`, which fail until the cached token expires). Other OIDC libraries follow the
JWKS's one-hour cache header.

## 11. Rotating the key-encryption key

Everything sealed under a key-encryption key records that key's id (`PID_KEK_ID`, `default`
when unset). pacod-id opens a record with the key of the same id, from `PID_KEK` or
`PID_KEK_OLD`, and seals new records under `PID_KEK`. Not exercised against the stack; the
rewrap itself is covered by the repository's tests.

1. Make a new key and choose a new id that was never used:

   ```sh
   openssl rand -base64 32
   ```

2. On every instance set `PID_KEK` to the new key, `PID_KEK_ID` to the new id, and
   `PID_KEK_OLD` to `<old id>=<old key>`. The old id is `default` if `PID_KEK_ID` was never
   set. Keys older still go after it, comma-separated. On Helm: `secrets.kek`, `kekId`,
   `secrets.kekOld`.
3. Restart every instance. They now seal under the new key and can read both.
4. Move the signing keys under the new key, from one of the restarted instances:

   ```sh
   pacod-id rewrap
   ```

   It prints "<n> keys moved under <new id>". It refuses while any instance seen in the last 45
   seconds runs with another key ("2 running instances do not have the current PID_KEK yet
   (ins_..., ins_...); restart them with it first"). Every instance reports its key's id and a
   check value (an HMAC that reveals nothing about the key) every 15 seconds; an instance that
   stopped cleanly removes itself, one that was killed counts for 45 seconds.
5. `rewrap` moves only the signing keys. Queued mails stay sealed under the key they were
   queued with, and are retried for up to about eight hours (section 13). Before removing the
   old key, check that none is left:

   ```sql
   SELECT count(*) FROM mail_outbox WHERE params->>'kek_id' = '<old id>';
   ```

6. Remove `PID_KEK_OLD` and restart.

Never give a new key an id that is already in use: ids are not checked for duplicates, and a
record sealed under the old key of the same id can no longer be read ("signing key ...: cannot
decrypt (is PID_KEK right?)").

## 12. Client secrets

Confidential and service clients authenticate with a secret (HTTP Basic or `client_secret_post`
at the token endpoint, HTTP Basic on the internal API); public clients have none. pacod-id
stores one HMAC per client, keyed with `PID_TOKEN_PEPPER` (table `clients`), never the secret.
A client gets its secret in one of two ways.

**From a variable (`secret_env`).** The configuration names the variable; its value (or file,
section 4) must have at least 32 characters, and the service does not start without it. Its
HMAC is written at every start. Generate it as hex: some client libraries put the secret into
the Basic header without form-encoding it, and pacod-id decodes that header as RFC 6749 says,
so a `+` (which `openssl rand -base64` produces) would be read as a space.

```sh
openssl rand -hex 32
```

**From `pacod-id clients secret <client>`**, for a client without `secret_env`:

```sh
pacod-id clients secret shop-web
```

It makes a random secret (43 characters, base64url), stores its HMAC, prints the secret once on
standard output and a note on standard error. It refuses a public client and a client whose
secret comes from `secret_env`. A confidential or service client with neither has no secret:
its calls fail with `invalid_client`, and nothing warns at start.

**Rotating a secret.** There is one secret per client and no overlap: the new one replaces the
old one for every instance as soon as it is stored.

- `secret_env`: change the value, restart pacod-id, give the product the new value. The old
  secret stops working when the first restarted instance starts.
- `clients secret`: run it again and give the product the printed secret. The old one stops
  working at once.

Plan a short interruption of that client's calls. Secrets set with `clients secret` must be set
again after a change of `PID_TOKEN_PEPPER`.

## 13. Mail

pacod-id sends these mails: address confirmation, password reset, invitation, "you already
have an account" (a sign-up with a taken address), password changed, address changed (to the
old address), new sign-in on another device, and "we signed you out everywhere" after a reused
refresh token. Each is rendered in the realm's brand, in every language of the realm's
`mail_locales` (Bulgarian, then English, by default), with the texts compiled into the binary.
Links are built from the realm's issuer, never from the request.

### SMTP

| `PID_SMTP_TLS` | Connection | Default port |
|---|---|---|
| `starttls` (default) | Plain connection upgraded with STARTTLS; refused if the server does not offer it. | 587 |
| `tls` | TLS from the first byte. | 465 |
| `none` | No encryption. Only for a sink on the same host or network, such as mailpit. | 25 |

Any other value of `PID_SMTP_TLS` stops the start ("PID_SMTP_TLS is starttls, tls or none, not
..."), and so does `PID_SMTP_USERNAME` without `PID_SMTP_PASSWORD` ("PID_SMTP_USERNAME is set
without PID_SMTP_PASSWORD"); both are checked only when `PID_SMTP_HOST` is set. A password
without a user name is ignored, and the server is used without signing in. The SMTP connection
has a timeout of 20 seconds. Mail through a real SMTP server was not exercised.

### The outbox

No request waits for SMTP. A mail is written to the table `mail_outbox`, its parameters sealed
under `PID_KEK`, and every instance sends what is due every 3 seconds. A failed attempt is
logged ("a mail was not sent", with the error and the template name) and retried
after 1, 2, 4, ... minutes, at most 4 hours apart. After 10 attempts, about eight hours after
the first, the mail is given up and its parameters are blanked. Sent mails are deleted after a
day, given-up ones after 7 days.

**When the SMTP server is down**, sign-in and sign-up go on and the mails wait. People who must
confirm their address cannot finish signing in until the mail arrives, and a reset link (60
minutes by default) may arrive already expired. What waits:

```sql
SELECT template, attempts, next_attempt_at, error
FROM mail_outbox WHERE sent_at IS NULL ORDER BY id DESC LIMIT 20;
```

### Without SMTP

Without `PID_SMTP_HOST` no mail is queued or sent, and:

- no address confirmation is asked for, whatever `require_verified_email` says; a sign-up signs
  in at once;
- an invitation's link is returned to the administrator who created it, to hand on;
- an administrator's "send reset link" and "send confirmation" answer 503 `email_unavailable`;
- a person's own reset request is answered as usual, and nothing is sent.

A realm with `self_signup = true` should not run without SMTP: anyone can then create accounts
with addresses that are not theirs.

## 14. Sessions and sign-out

A sign-in creates a session in the realm (12 hours, or 7 days with "keep me signed in";
`session_hours`, `remember_me_days`). A browser's refresh tokens end with their session.

**Ending sessions.**

- The person: "Sign out" in the product, which goes through `/oidc/logout`; or the device list
  on `<issuer>/account/sessions`, which ends one session or all others.
- A product's server, on the internal port:

  ```sh
  curl -X POST -u '<client id>:<client secret>' \
    http://<internal address>:8071/internal/v1/sessions/<sid>/revoke
  ```

  It answers 204. A confidential client (HTTP Basic, as above) may end only sessions in which
  it holds or held a grant, otherwise 403. A service client, with a token from the
  `client_credentials` grant as `Authorization: Bearer`, may end any session.
- A refresh token presented again after it was replaced (outside a 30-second grace) ends every
  session and grant of the account, and the owner gets a mail. A token pacod-id never issued,
  even one naming a real grant, is only refused.

**What the person sees.** In the product that ended the session, they are signed out at once.
In other products of the same session, the next refresh fails and they are asked to sign in
again; with back-channel logout, or by following the feed, a product ends its own session
sooner.

### Back-channel logout

When a session ends, every client that holds or held a grant in it and has a
`backchannel_logout_uri` receives a logout token: a JWT (`typ: logout+jwt`) with `sid`, `sub`
and the logout event, valid for two minutes, signed afresh for each attempt. pacod-id posts it
form-encoded as `logout_token=<jwt>`, with the header `Origin: <issuer origin>`, waits at most
5 seconds and follows no redirect. A 2xx answer counts as delivered.

Failures are retried after 5, 10, 20, 40 ... seconds; an attempt that fails more than 10
minutes after the logout ends the retries. That makes about eight attempts. Each failure is
logged as a warning, "a back-channel logout was not accepted" with the client and the error
(`HTTP 403`, a timeout), or "a back-channel logout could not be signed"; a token that cannot be
signed fails only its own delivery. There is no metric. The failures are in the table
`backchannel_outbox`:

```sql
SELECT client_id, attempts, last_error, created_at, done_at
FROM backchannel_outbox WHERE last_error <> '' ORDER BY id DESC LIMIT 20;
```

The usual failure is a SvelteKit application refusing the cross-site form post (`HTTP 403`)
before its handler runs. Add the issuer's origin to `csrf.trustedOrigins` in its SvelteKit
configuration; `@pacod/id-sveltekit` documents this.

## 15. Health, metrics and logs

### Probes

Both are on port 9170.

| Endpoint | Answers 200 when | Use for |
|---|---|---|
| `/healthz` | The process runs (`ok`). It checks nothing else. | Liveness. |
| `/readyz` | Start-up is done (migrations, keys, catalogue, first administrator), the instance is not shutting down, the database answers `SELECT 1`, and an RS256 signing key is loaded (`ready`). Otherwise 503 `not ready`. | Readiness, load balancers, Docker's health check. |

`/readyz` does not check the hosted pages, SMTP, Google or the feed's listening connection.
A missing page build is visible only in the log and in the pages' 503 (section 18).

### Metrics

`/metrics` is in the Prometheus text format. pacod-id defines two metrics:

| Metric | Labels | What |
|---|---|---|
| `pid_http_requests_total` | `listener` (`public`, `internal`), `status` (`2xx`, `4xx`, ...) | Requests answered. |
| `pid_http_request_seconds` | `listener` | Duration of requests. No buckets are configured, so it is a summary with the quantiles 0, 0.5, 0.9, 0.95, 0.99, 0.999 and 1. |

There is no metric for the mail or logout outboxes, the throttles, or the keys; use the
queries of sections 13 and 14, and the log.

### Logs

The log follows the shared pacod convention (`pacod-demo/docs/LOGGING.md`): one event per line
on standard output, the same keys in every product.

- **Level**, `PID_LOG` (default `info,sqlx=warn`): `debug`, `info`, `warn` or `error` in any case
  (`warning` is `warn`), or an `EnvFilter` directive, for example `debug,sqlx=warn` to see more.
  A plain `debug` also shows sqlx's queries; add `,sqlx=warn` to keep them out. A value that is
  neither a level nor a valid directive (a single word must be a level) gives the default, and
  the first line of the log is a warning that names it.
- **Format**, `PID_LOG_FORMAT`: `json` (the default) or `text`, in any case; any other value gives
  JSON. The development and conformance stacks set `text` (`${PID_LOG_FORMAT:-text}` in their
  compose files).
- **Service**, `SERVICE_NAME` (default `pacod-id`): the compose service name, which the compose
  files set.

Each JSON line holds `timestamp` (RFC 3339, UTC, milliseconds), `level` (lower case), `service`,
`logger` (the Rust module, or the target of a library's line) and `message`, in this order, then
the event's fields at the top level with their JSON types (an error's text in `error`). A field
named like one of the five keys is written with a trailing underscore (`message_`). There are no
host names, process ids, nested `fields` objects or colour codes.

```json
{"timestamp":"2026-10-23T06:41:07.695Z","level":"info","service":"pacod-id","logger":"pacod_id::app","message":"pacod-id is up","public":"0.0.0.0:8070","internal":"0.0.0.0:8071","metrics":"0.0.0.0:9170"}
{"timestamp":"2026-10-23T06:52:13.004Z","level":"warn","service":"pacod-id","logger":"pacod_id::mail","message":"a mail was not sent","error":"connection refused","template":"verify_email"}
```

Text has the same content on one line: time, level (upper case, five characters wide), service,
`logger:`, message, then `key=value` (values with spaces in quotes), without colour codes:

```text
2026-10-23T06:41:07.695Z INFO  pacod-id pacod_id::app: pacod-id is up public=0.0.0.0:8070 internal=0.0.0.0:8071 metrics=0.0.0.0:9170
2026-10-23T06:52:13.004Z WARN  pacod-id pacod_id::mail: a mail was not sent error="connection refused" template=verify_email
```

The compose files rotate every container's log (Docker's `json-file`, 5 files of 10 MB).

pacod-id writes no line per request. It logs its start ("pacod-id is up", with the three
addresses), the hosted pages loaded or missing, key rotations (the key ids), catalogue
changes, the first administrator (its address and roles), account deletions, and warnings and
errors: mails not sent, back-channel logouts not delivered, failures of the background work,
Google unreachable, the feed's listening connection lost, and internal errors with their error
text. None of these log
statements writes a password, a token, a client secret or a key; errors from PostgreSQL, the
SMTP server or Google are logged with the text those report.

Sign-ins, sign-outs, failures, account and role changes, key rotations and administrators'
actions go to the audit log in the database (table `audit_log`, kept 400 days), readable by
holders of `id.admin` and `id.auditor` on `<issuer>/admin/audit`.

## 16. Backup and restore

Not exercised.

### What to back up

| What | How | Why |
|---|---|---|
| The database `pacod_id` | `pg_dump`, below | Everything pacod-id knows. |
| `PID_KEK`, `PID_KEK_ID`, and the keys still in `PID_KEK_OLD` | Separately, **never with the dump** | Without them the dump's signing keys and queued mails cannot be read; with them, the dump is enough to sign tokens. |
| `PID_TOKEN_PEPPER` | Separately, with the KEK | Without it, `clients secret` secrets and service tokens must be issued again (section 4). |
| The configuration file, client, Google and SMTP secrets, `PID_LEGACY_PEPPERS` | With your deployment's secrets | Needed to start. |

The dump holds password hashes, sealed keys and HMACs: keep it as confidential as the service.

```sh
pg_dump --format=custom --file pacod-id-$(date +%F).dump \
  'postgres://pacod_id:<password>@<host>:5432/pacod_id'

dc exec -T postgres pg_dump -U postgres --format=custom pacod_id > pacod-id-$(date +%F).dump
```

### Restoring

1. Stop every instance.
2. Create the role and an empty database (section 5), then restore as the owner:

   ```sh
   pg_restore --no-owner --dbname 'postgres://pacod_id:<password>@<host>:5432/pacod_id' \
     pacod-id-<date>.dump
   ```

3. Start pacod-id with the `PID_KEK` (and `PID_KEK_ID`, `PID_KEK_OLD`) and `PID_TOKEN_PEPPER`
   that were in use when the dump was made, or a later set that still holds those keys.
4. Check `/readyz`, sign in, run `pacod-id keys list`.

A restore brings back the state of the dump's time:

- Accounts, sessions, grants and bindings created since are gone; accounts deleted and sessions
  or keys revoked since are back. Revoke again what was revoked for a reason.
- A refresh token rotated since the dump is unknown to the restored grant (a secret it never
  had): its next use is refused with `invalid_grant`, without ending other sessions or sending
  a mail. Expect people to sign in again.
- Products that follow the feed may hold a position ahead of the restored feed. Asking for the
  changes after it, a follower gets 410 and loads the snapshot again, but once new changes have
  carried the feed past its position, it misses those numbered up to that position. Restart
  their followers so they load the snapshot again (the Rust and Python clients load it when
  they start).

Accounts deleted before the dump's time do not come back; deleted accounts stay in older dumps
until those expire.

## 17. Upgrades

Not exercised: pacod-id has not been released yet (version 0.1.0 is the development line,
with one migration).

1. Read the release notes.
2. Take a backup (section 16).
3. Check the configuration with the new image: `check-config` (section 3). Its rules can grow
   stricter from one version to the next (a service client with `token_exchange` must name an
   `audience`; `password_min_length` is at least 8), and a file that fails them stops every
   new instance at start.
4. Roll out. Compose: set the new image and `docker compose ... up -d --wait`. Helm: set
   `image.tag` and `helm upgrade`. The first new instance migrates the database at start
   (section 5); with several instances, run `pacod-id migrate` from the new image first if you
   prefer.
5. Watch `/readyz` and the log.

The rule for migrations is that they only add (`0001_init.sql` states it), so instances of the
previous version should keep working on the migrated database while the rollout runs. An
instance of the previous version that **starts** after the migration refuses to start
("migration <n> was previously applied but is missing in the resolved migrations"): finish the
rollout rather than restarting old pods.

**Rolling back** the image after a release that added a migration is therefore not possible:
restore the backup of step 2.

## 18. Troubleshooting

| Symptom | Cause | What to do |
|---|---|---|
| Exits at start: "PID_KEK is not set. Generate one with `openssl rand -base64 32` ..." (or `PID_DATABASE_URL`, `PID_TOKEN_PEPPER`) | A required secret is missing, or its file is not where `NAME_FILE` or `PID_SECRETS_DIR` says. | Section 4. |
| Exits at start: "client X: NAME (its secret) is not set" or "... must have at least 32 characters" | A `secret_env` variable is missing or short. | Section 12. |
| Exits at start: "realm X: NAME (its Google client secret) is not set" | A `google.secret_env` variable is missing. | Set it, or remove the block. |
| Exits at start: "PID_EMAIL_FROM_ADDRESS is needed with PID_SMTP_HOST" | SMTP host without sender. | Set the sender. |
| Exits at start: "PID_SMTP_TLS is starttls, tls or none, not ..." or "PID_SMTP_USERNAME is set without PID_SMTP_PASSWORD" | A mistyped TLS mode, or an SMTP user name without its password. | Section 13. |
| Exits at start naming `PID_CONFIG` and a realm or client, for example "client X: a service client that exchanges tokens names the audience of its product's tokens (e.g. pacod-gw)" or "realm X: password minimums must be 8 to 128 ..." | The configuration breaks a rule of section 3, perhaps one a newer version added. | Correct the file and check it with `check-config` (section 3). |
| Exits at start: "loading the signing keys: signing key K: cannot decrypt (is PID_KEK right?)", or "... key-encryption key \"X\", which is not configured" | `PID_KEK` or `PID_KEK_ID` changed without the old key in `PID_KEK_OLD`. | Put the old key back (section 11). If it is lost: section 4. |
| Exits at start: "migration N was previously applied but is missing ..." | An older image against a newer database. | Use the newer image, or restore (section 17). |
| Exits at start: "migration N was previously applied but has been modified" | An applied migration file was edited, which section 5 rules out; the database still holds the old file's checksum. | Restore the migration as it was applied and put the change in a new migration. A development database you can lose: `dc down -v` and start again. |
| Exits at start: "connecting to PostgreSQL" or "migrating the database" | Database unreachable, wrong password, or the role may not create tables. | Section 5. |
| Exits at start: "binding the public listener to ..." | Port in use, or a bad `PID_*_LISTEN`. | Free the port or change the variable. |
| Exits at start: "PID_BOOTSTRAP_ADMIN_..." | The first administrator cannot be created: realm, password or roles. | Section 9. |
| `/readyz` answers 503 | Start-up not finished, the database does not answer, no RS256 key loaded, or the instance is stopping. | Read the log; check PostgreSQL. |
| Pages answer 503 "The sign-in pages are not installed (PID_UI_DIR)." while `/readyz` is ready | `PID_UI_DIR` has no `index.html`; the log says "no hosted pages". | Point `PID_UI_DIR` at the build (`/usr/share/pacod-id/ui` in the image) and restart. |
| 404 "No sign-in service is configured at this address." or "no realm is served at ..." | The `Host` header does not match a realm's issuer. | Pass the original host through the proxy; check the issuer's host and port. |
| 403 `cross_origin` "requests come from the sign-in pages only" | The browser's `Origin` differs from the issuer's origin: `http` against `https`, another port, or a proxy that rewrites it. | Make the issuer the exact URL people use. |
| 429 "too many sign-ups from this network" or "too many attempts; try again in 15 minutes" for many people at once | Behind a proxy without `PID_TRUSTED_PROXIES`, everybody shares one address. | Section 8. |
| `invalid_client` from the token endpoint | Wrong secret; no secret set (no `secret_env`, no `clients secret`); a `+` in a secret sent unencoded; `PID_TOKEN_PEPPER` changed. | Section 12. |
| Mails do not arrive | No `PID_SMTP_HOST` (nothing is queued); the SMTP server refuses ("a mail was not sent" in the log); the per-address limit dropped them; the recipient's spam filter. | Section 13: query `mail_outbox`. |
| A product is not told about sign-outs | Its back-channel endpoint fails, often `HTTP 403` from SvelteKit's CSRF check; the log says "a back-channel logout was not accepted". | Section 14: query `backchannel_outbox`. |
| A product's log says "pacod-id did not end the session (HTTP 404); is internalUrl pacod-id's internal listener?" | `@pacod/id-client`'s `internalUrl` points at the public listener, which does not serve `/internal/v1`: the SSO session lives on, and the next sign-in is silent. | Set `internalUrl` to the internal listener, `http://<host>:8071`. |
| "Google sign-in is not available just now" (`google_unavailable`) | pacod-id cannot read the provider's discovery document: no outbound HTTPS, or the document names another issuer. | Check egress to Google and the block's `issuer`. |
| `pacod-id rewrap` refuses: "... running instances do not have the current PID_KEK yet" | An instance runs with the old key, or one was killed less than 45 seconds ago. | Restart the named instances with the new key, or wait. |
| Products refuse new tokens after `keys rotate`, or still accept a revoked key | Their cached JWKS. The Rust and Python libraries take a new key within 30 seconds and drop a revoked one at their hourly read. | Section 10: restart them. |
| `pacod-id keys rotate --revoke <kid>` fails with "no key <kid>: see `pacod-id keys list`; nothing changed" | The key id is mistyped, or the key is gone already. | Run `keys list` and repeat with the right id. |

## 19. Taking over the gateway's people

A gateway that signed people in itself (`PGW_AUTH=local`) moves them to pacod-id once:

1. On the gateway: `pacod-gw-core identity export --out gw-identity.json`. The file holds
   password hashes and token hashes: keep it like a secret, and delete it afterwards.
2. Here, with the gateway's pepper in `PID_LEGACY_PEPPERS` (`gw:<PGW_KEY_PEPPER>`, so that its
   `pgwa_` admin tokens keep working): `pacod-id import gw gw-identity.json --dry-run`. The
   report counts what would be created, changed and left as it is, lists what is skipped and
   why, and lists the conflicts. A conflict is most often an address pacod-id has already under
   another account, such as the first administrator of section 9 created with the gateway
   admin's address: delete that account (or give it another address) and run the dry run again.
   The gateway's accounts are never merged with others.
3. `pacod-id import gw gw-identity.json`. Ids stay (`usr_…`, `tok_…` as service accounts,
   `rb_…`, `inv_…`); passwords, admin tokens and invitation links keep working. All or nothing.
   Running instances take the gateway's catalogue, when the import brought it, within ten
   seconds, or at once when the gateway pushes it at its start.
4. Switch the gateway to `PGW_AUTH=oidc`. Everyone signs in once more, now here.

Until the gateway's `identity finalize`, it can return to its own sign-in: its `identity
rollback` asks `GET /internal/v1/imports/gw` and refuses once anything was written here since
the import, because that would be lost. A second import is refused for the same reason. After
such a rollback, a new export and import make pacod-id match the gateway again: what the
gateway removed meanwhile is removed or disabled here too. `import gw` does not migrate the
database; run it against one the service has brought up to its version.

