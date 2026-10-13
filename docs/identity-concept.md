# PACOD Identity (pacod-id): shared sign-in and access concept (design v0.1)

| Field | Value |
|---|---|
| Date | 2026-10-12 |
| Status | Concept, draft for review |
| Inputs | pacod-gw: [concept](ai-gateway-concept.md) sections 5.4 and 19, [architecture](architecture.md), [threat model](threat-model.md) and the code under `crates/pacod-gw-core/src/mgmt` and `ui/`. PACOD: `pacod-api` (`app/api/routes_auth.py`, `app/guards/auth.py`, `app/services/auth_service.py`, `admin_api/`) and the frontend's server-side session (`pacod-ui/frontend/src/lib/server/session.ts`, `hooks.server.ts`). |
| Decisions already taken | (1) One account pool with single sign-on, served by a central identity service that both products use through OpenID Connect. (2) A standalone gateway keeps working: the identity service ships with it as a companion container. (3) We build the service ourselves in Rust, extracted from pacod-gw plus the parts PACOD does better, and check it with the OpenID conformance suite. (4) Vanga is in scope: the same account pool, its own brand and sign-in host, and the Android app as a native OIDC client. Decided later the same day: every item of section [19.2](#192-decisions). |
| Not available | Conformance-suite results, the Android app's source, load measurements. |
| Marker | "(to verify)" marks a statement about a standard or a third party that must be checked before it is built on. |
| Location | The working copy is now `docs/concept.md` in the pacod-id repository. This copy stays as decided on 2026-10-12. |

---

## 1. Summary

### 1.1 What it is

pacod-id is the one place where people and machines sign in, and where it is recorded what they may do, for every PACOD product: the AI gateway (pacod-gw), PACOD itself and its consumer brand Vanga.

It has four parts:

- **A Rust service** (`pacod-id`) that is an OpenID Connect provider for our own applications, keeps the accounts, organisations and role bindings, and publishes changes to them as a feed.
- **A hosted sign-in and account UI**, the same page for every product, styled per brand (PACOD, Vanga, Gateway) and available in English, Bulgarian, German and Russian.
- **Client libraries** for the three stacks the products use (Rust, Python, SvelteKit). They check tokens, follow the feed and evaluate permissions locally.
- **A design package** (`@pacod/ui`) with the tokens and components both consoles and the hosted UI draw from.

Products keep their own data and their own consumer entitlements. What moves is who someone is, how they prove it, and which roles they hold where.

### 1.2 Requirements and where they are answered

| # | Requirement | Section |
|---|---|---|
| 1 | Authentication split into a module that several products use | [3](#3-architecture), [11](#11-client-libraries) |
| 2 | Authorization split out as well | [7](#7-authorization) |
| 3 | The same sign-in mechanism everywhere | [5](#5-sign-in-protocol), [6](#6-accounts) |
| 4 | The same look and feel | [10](#10-hosted-ui-and-the-shared-design-package) |
| 5 | The same backend infrastructure | [3](#3-architecture), [16](#16-deployment-and-operations) |
| – | One account and single sign-on across products (decision 1) | [4](#4-realms-brands-and-clients), [5.4](#54-sso-session-remember-me-and-devices) |
| – | A standalone gateway keeps working (decision 2) | [16.1](#161-standalone-gateway) |
| – | Own Rust service, checked with the conformance suite (decision 3) | [3](#3-architecture), [5.7](#57-conformance) |
| – | Vanga and its Android app (decision 4) | [4](#4-realms-brands-and-clients), [5.6](#56-the-android-app), [14](#14-integration-pacod-and-vanga) |
| – | No product gets slower at taking access away than it is today | [7.5](#75-revocation-targets-and-a-stale-feed) |
| – | One copy of the permission rules, not one per language | [7.6](#76-one-copy-of-the-rules) |

### 1.3 The decisions that shape the design

1. **pacod-id has three faces.** An OIDC provider for first-party clients, an account and admin API, and a change feed for authorization data. The OIDC face is standard, so clients do not depend on pacod-id being the server behind it. The feed is pacod-id's own and is documented as such.
2. **Access tokens say who, never what.** They carry identity and session, not roles. Products evaluate roles from a local copy of the bindings, kept current by the feed. That is how a role change or a revoked session takes effect within 2 seconds while tokens still live 5 minutes. The one exception is Open-WebUI, which can only read roles from its sign-in token (section [14.2](#142-frontend)).
3. **One rules engine, three ports.** A small, pure evaluator exists in Rust, Python and TypeScript. Each product declares its roles and permissions in a manifest. Shared YAML test vectors run against all three ports, so they cannot drift.
4. **Organisations, memberships, invitations and role bindings live in pacod-id.** Products register their own sub-resources (gateway workspaces, PACOD profiles) as scopes, so bindings can point at them.
5. **Products keep their own entitlements.** PACOD's profile assignments and quotas and the gateway's data-plane keys stay product data. pacod-id records who may administer or operate what.
6. **PostgreSQL only.** Throttles, login flows, refresh grace, the mail queue and the feed all live in PostgreSQL. No Redis, so a standalone gateway gains one container and no new store.
7. **The hosted UI is a static SvelteKit app served by the Rust binary.** One container, one process, no Node at runtime.
8. **The design package depends on Svelte only, never on SvelteKit.** It works in a plain-CSS app (the gateway) and a Tailwind app (PACOD) alike.
9. **Migration keeps ids, and PACOD keeps its sessions.** Gateway user ids become account ids and PACOD keeps its user ids and maps them. PACOD's and Vanga's existing refresh tokens keep working, so their users are not signed out. Gateway console users sign in once more at the cutover; the gateway's admin tokens keep working throughout.

---

## 2. Goals, non-goals and first users

### Goals

- One account per person across the gateway console, PACOD and Vanga, and one sign-in for the products of one brand.
- The same sign-in screens, emails and account pages in every product, differing only in brand.
- Permissions defined once per product and evaluated the same way in Rust, Python and TypeScript.
- Revocation at least as fast as the gateway's today, in both products.
- A standalone gateway installation that stays small: one more container, no new kind of store.
- Standard protocols at the edges (OIDC, OAuth 2.0), so a product can be pointed at another provider later and a customer's own identity provider can be connected later.

### Non-goals for the first release

- Third-party clients, dynamic client registration, consent screens. Every client is ours.
- SAML. Organisation SSO comes later and starts with OIDC upstreams (section [6.8](#68-organisation-sso-later)).
- Fine-grained, per-object permissions (Zanzibar-style relations). Role bindings at scopes cover both products.
- Billing, plans and quotas. They stay in the products.
- Moving product data. The gateway's configuration and PACOD's profiles stay where they are.

### First users

| User | Signs in to | Today |
|---|---|---|
| Gateway operators and organisation admins | the gateway console | local accounts in the gateway |
| Automation against the gateway's management API | `pgwa_` admin tokens | admin tokens in the gateway |
| PACOD users and admins | www.pacod.io | PACOD accounts, Google sign-in |
| Vanga users | vanga.info and the Android app | the same PACOD accounts, Vanga-branded pages |

### Starting point

Both products already keep tokens away from the browser (a server-side session behind an httpOnly cookie), both consoles are Svelte 5, and the gateway's look was taken from pacod-ui (`ui/src/app.css`, `ui/src/lib/components/AuthCard.svelte`). The two implementations differ in almost everything else:

| | pacod-gw | PACOD |
|---|---|---|
| Stack | Rust/axum core, SvelteKit 3 console | Python/FastAPI, SvelteKit 2 frontend |
| Passwords | Argon2id, at least 12 characters (`mgmt/resources/access.rs:23`) | bcrypt cost 12, at least 8 characters (`app/services/auth_service.py:44`) |
| Session | Opaque `pgws_` bearer, looked up in PostgreSQL on every request (`mgmt/auth.rs:148`) | HS256 JWT for 30 minutes (`auth_service.py:22-23`) plus a rotating refresh token in `device_sessions` |
| Roles | Platform, organisation and workspace scopes, read from the in-memory snapshot (polled every second, `snapshot/reload.rs:18`) | `admin`, `user`, `superuser`, taken from the JWT claim without a database check (`app/guards/auth.py:74`) |
| Revocation | Sessions at once; roles within about 1 s | Up to 30 minutes for roles, disabled accounts and revoked devices |
| Has | Invitations, last-admin rule, dummy-hash timing, per-email lockout | Google sign-in, refresh reuse detection, device list, self-signup, four languages |
| Lacks | Google sign-in, refresh rotation, languages | Organisations, invitations, sign-in throttling, `iss` and `aud` in tokens |
| Storage | Users, role bindings and admin tokens are JSON documents in `config_objects`, part of the configuration snapshot | `users`, `roles`, `user_roles`, `oauth_accounts`, `device_sessions` tables; email tokens in Redis |

The permission rules are already written twice in the gateway and disagree: `ui/src/lib/access.ts:47` lets only platform admins and platform auditors read the audit log, while `mgmt/auth.rs:104` also admits organisation admins and organisation auditors for their organisation. PACOD has two functions called `require_admin` with different meanings (`app/api/dependencies.py:95`: admin only; `admin_api/dependencies.py:67`: admin or superuser).

---

## 3. Architecture

```text
                         id.pacod.io · id.vanga.info · id.<customer>
   browsers ───────────────► ┌──────────────────────────────┐
   Android app (AppAuth) ───►│  pacod-id  :8070 public      │  /oidc/*  hosted UI  /ui-api/v1
                             │  (Rust)    :8071 internal    │  catalogue, scopes, feed, admin, introspection
   gateway console (BFF) ◄──►│            :9170 metrics     │──► SMTP, Google
   PACOD / Vanga web (BFF)◄─►└──────────────┬───────────────┘
                                            │
            ┌───────────────────────────────┼─────────────────────────┐
            │ snapshot + long-poll feed      │ facades (token exchange)│
            ▼                                ▼                         ▼
   pacod-gw-core (Rust)              pacod-api / admin-api (Python)   PostgreSQL
   pacod-id-client crate             pacod_id package                 schema or database pacod_id
```

### 3.1 Components

| Component | Where | Responsibility |
|---|---|---|
| **Identity service** | `pacod-id/crates/pacod-id` | OIDC provider, sign-in flows, accounts, Google federation, organisations, scopes, role bindings, invitations, service accounts, mail, identity audit log, signing keys, the authorization feed. |
| **Hosted UI** | `pacod-id/ui` | Sign-in, sign-up, password reset, email verification, invitations, the account pages (profile, security, devices, connections, organisations) and identity administration. Static files served by the service. |
| **Rules engine** | `pacod-id-authz` (Rust), `pacod_id.authz` (Python), `@pacod/id-authz` (TS) | Manifests, scope paths, bindings, `allowed`, `has_role`, `touches`. No input or output, only evaluation. |
| **Product libraries** | `pacod-id-client` (Rust), `pacod_id` (Python), `@pacod/id-client` and `@pacod/id-sveltekit` (TS) | Token checks against JWKS, the bindings cache and feed follower, introspection of service tokens, the BFF sign-in flow. |
| **Design package** | `@pacod/ui` | Design tokens, brand themes, sign-in and account components. |
| **Store** | PostgreSQL | All identity state, in its own schema or database and with its own database role. |

### 3.2 Rules that keep the parts separate

- Only pacod-id reads or writes identity tables. Products learn about identity through tokens, the feed and the admin API.
- The browser talks to pacod-id only on its own host, for sign-in and account pages. Product pages never call pacod-id from the browser.
- Products never see a password, a refresh token of another product, or a signing key.
- Product data stays in the product. pacod-id stores a scope's id, type, parent and display name, nothing else about it.
- A product's request path does not wait on pacod-id. Tokens are checked locally against cached keys and roles are evaluated from the cached bindings. pacod-id is needed to sign in, to refresh and to change identity data.
- The gateway's data plane does not depend on pacod-id at all. Data-plane keys stay in the gateway.

### 3.3 Listeners

| Listener | Port | Serves | Reachable from |
|---|---|---|---|
| Public | 8070 | `/.well-known/openid-configuration`, `/oidc/*`, `/federation/*`, the hosted UI, `/ui-api/v1/*` | the internet, through the reverse proxy |
| Internal | 8071 | `/internal/v1/*` (catalogue, scopes, organisations, feed, introspection, session revocation), `/admin/v1/*` | product backends and BFFs only |
| Metrics | 9170 | `/metrics`, `/healthz`, `/readyz` | Prometheus and health checks |

This copies the gateway's split between public traffic, management and metrics (concept section 3.3). The ports avoid the gateway's own 8080, 8081, 8090 and 9100.

### 3.4 Repository and distribution

A new repository, `pacod-id`, under Apache-2.0. It must be public and Apache-2.0 because the gateway, which is Apache-2.0, ships it in its compose file and Helm chart and must build from public sources.

```text
pacod-id/
  Cargo.toml                         workspace
  crates/pacod-id/                   the service: oidc, flows, accounts, federation, authz,
                                     service_accounts, mail, audit, keys, throttle, jobs; migrations/
  crates/pacod-id-authz/             the rules engine
  crates/pacod-id-client/            the product-side Rust library
  python/                            pyproject.toml, src/pacod_id/
  packages/{authz,client,sveltekit,ui}/   pnpm workspace, @pacod/*
  ui/                                hosted UI (SvelteKit, adapter-static, Paraglide)
  policy/engine-vectors/             YAML vectors for the engine's semantics
  conformance/                       OpenID conformance-suite plans and the CI job
  deploy/{docker,compose,helm/pacod-id}/
  docs/{concept.md,threat-model.md,runbook.md,api.md}
```

| Artefact | Channel | Pinned by |
|---|---|---|
| Rust crates | git tag first (`pacod-id-client = { git = "…/pacod-id", tag = "v0.1.0" }`), crates.io once stable | `Cargo.lock`; the gateway's `deny.toml` allows the source |
| npm packages | npmjs, scope `@pacod` | exact versions in `package-lock.json` |
| Python package | `uv` git dependency (`pacod-id @ git+…@v0.1.0#subdirectory=python`), PyPI later | `uv.lock` |
| Container | `ghcr.io/<org>/pacod-id:<version>`, signed with cosign and shipped with an SBOM, as the gateway's releases are | image digest in the gateway's compose file and Helm subchart |

Libraries and service talk over a versioned contract: the claims profile v1 and `/internal/v1`. Each library is tested against the two latest minor versions of the service, and the release notes carry a compatibility table.

The code taken from pacod-gw:

| From pacod-gw | Becomes |
|---|---|
| `crypto/password.rs` | password hashing, with bcrypt verification added for PACOD's hashes |
| `crypto/envelope.rs` | envelope encryption of signing keys and other secrets |
| `crypto/keys.rs` (generation, base62, redaction) | token formats |
| `mgmt/session.rs` (lockout logic, dummy hash) | sign-in flows, now backed by PostgreSQL |
| `mgmt/account.rs` | email verification and password reset |
| `mgmt/members.rs` | invitations and the last-admin rule |
| `mail.rs` | the mail module, with templates instead of `format!` strings |
| `model/access.rs` | the gateway's manifest and the engine's binding types |

From PACOD it takes the design of refresh rotation with reuse detection and a grace window (`app/api/routes_auth.py:523` onward), the device list and Google sign-in, rebuilt in Rust.

---

## 4. Realms, brands and clients

### 4.1 Realms and hosts

A **realm** is one issuer host. It has its own brand, single sign-on cookie, Google OAuth client, mail sender, languages and sign-up policy.

| Realm | Issuer | Brand | Used by |
|---|---|---|---|
| `pacod` | `https://id.pacod.io` | PACOD | PACOD web, and the gateway console on the PACOD platform |
| `vanga` | `https://id.vanga.info` | Vanga | Vanga web and the Android app |
| `gateway` | `https://id.<customer domain>` (`https://id.localhost` in development) | Gateway | a standalone gateway |

**There is one account pool.** An account's id (`sub`) is the same in every realm of an installation. A product that accepts tokens from two realms (PACOD's API accepts both `pacod` and `vanga`) can rely on that.

**Single sign-on works within a realm**, because the session cookie belongs to one host. Signing in to PACOD signs you in to the gateway console on the same platform. Signing in to Vanga does not sign you in to PACOD: the domains differ, and that is wanted, since Vanga is a consumer brand.

**The realm is found from the request.** Browser endpoints (`/oidc/authorize`, `/oidc/logout`, the hosted UI) use the Host header, checked against the configured realms. Back-channel endpoints (`/oidc/token`, `/oidc/revoke`) use the authenticated client, because every client belongs to exactly one realm. A BFF can therefore call `http://id:8070/oidc/token` inside the network and still receive tokens with `iss` set to the realm's public URL.

### 4.2 Clients

Clients are static configuration in `PID_CONFIG` (a TOML file), written to the `clients` table at start. There is no dynamic registration and no consent screen.

| Client | Realm | Kind | Access token `aud` | Grants |
|---|---|---|---|---|
| `pacod-gw-console` | `gateway` (standalone) or `pacod` (platform) | confidential, BFF | `pacod-gw` | authorization code + PKCE, refresh |
| `pacod-gw-core` | – | service | – | client credentials (internal API), token exchange |
| `pacod-web` | `pacod` | confidential, BFF | `pacod-api` | authorization code + PKCE, refresh |
| `vanga-web` | `vanga` | confidential, BFF | `pacod-api` | authorization code + PKCE, refresh |
| `vanga-android` | `vanga` | public, native | `pacod-api` | authorization code + PKCE, refresh |
| `pacod-api` | – | service (api and admin-api) | – | client credentials, token exchange |
| `pacod-webui` (phase 3b) | `pacod` | confidential | `pacod-webui` | authorization code + PKCE; the only client whose id_token carries `roles` (section [14.2](#142-frontend)) |

The audience names the product, not the brand. PACOD web, Vanga web and the Android app call the same API, so their tokens share `pacod-api`. The API reads the `client_id` claim to tell them apart, which replaces trusting the `X-Pacod-Site` header for attribution.

Per client: exact redirect URIs (no wildcards), post-logout redirect URIs, the back-channel logout URI, access-token lifetime (2 to 15 minutes, default 5), refresh policy, whether self-signup is offered, and the sign-in methods shown.

### 4.3 Brand themes

A brand is a theme of the shared design package (section [10.2](#102-tokens-and-themes)) plus a logo, a product name, a mail sender and the legal links (terms, privacy). The hosted UI picks the brand from the realm; a client may narrow it (the gateway console on the PACOD platform shows "PACOD AI Gateway" in the heading, in the PACOD theme).

---

## 5. Sign-in protocol

### 5.1 Flows

- **Authorization code with PKCE (S256) for every client**, confidential and public alike. The response carries `iss` (RFC 9207). The implicit, hybrid and password grants are not offered.
- **Supported parameters:** `prompt=login|none|create` (Prompt Create 1.0, for "Create account" links), `max_age`, `login_hint`, `ui_locales`, and from phase 4 `acr_values`.
- **The sign-in transaction:**
  1. `/oidc/authorize` checks the client, the redirect URI (exact match) and the PKCE challenge.
  2. If the browser has a usable SSO session that satisfies `prompt` and `max_age`, a code is issued at once.
  3. Otherwise an `auth_flows` row is created, the cookie `__Host-pid_flow` binds the flow to this browser (against login CSRF), and the browser goes to `/login?flow=<id>`.
  4. The hosted UI reads `GET /ui-api/v1/flows/{id}` and posts the credentials to `POST /ui-api/v1/flows/{id}/password` or starts Google with `POST /ui-api/v1/flows/{id}/google`. The answer is `{next: "redirect", to}`, `{next: "verify_email"}`, `{next: "change_password"}` (section [6.1](#61-passwords)) or, from phase 4, `{next: "mfa"}`.
  5. `/ui-api` accepts only `Content-Type: application/json` with an `Origin` equal to the realm's host.
- **Token exchange (RFC 8693), first-party only.** A product's service client may exchange a token it received from a caller for a 60-second token with `aud=pacod-id` and `act.sub` naming the product. The subject token must be a user access token whose `aud` is that product, or one of the product's callers' service tokens. Products use it for their admin facades (sections [13.2](#132-facades) and [14.1](#141-api-and-admin-api)): pacod-id then decides on the end user's own bindings.
- **Client credentials** for product services calling the internal API.

### 5.2 Tokens and lifetimes

| Artefact | Format | Lifetime |
|---|---|---|
| Access token | JWT per RFC 9068 (`typ: at+jwt`), RS256 (ES256 for a client that asks for it), with `kid` | 5 minutes (2 to 15 per client) |
| id_token | JWT, RS256 (ES256 for a client that asks for it) | 5 minutes |
| Refresh token | opaque `pidr_<grant id>_<43 base62>`, stored as SHA-256, rotated on every use | web: ends with the SSO session (12 hours, 7 days with "keep me signed in"); Android: 7 days from the last use, renewed by each refresh, 365 days at most |
| Refresh grace | the same successor is returned for a repeated refresh | 30 seconds |
| Authorization code | random, stored as SHA-256, single use | 60 seconds |
| Sign-in flow | `auth_flows` row | 15 minutes |
| Service token | opaque `pidt_<id>_<secret>`, HMAC with `PID_TOKEN_PEPPER` | optional expiry |
| Email verification link | `account_tokens` row, single use | 24 hours |
| Password reset link | `account_tokens` row, single use | 60 minutes |
| Invitation | `invitations` row | 7 days |

The verification, reset and invitation lifetimes are the gateway's current defaults (`verification_link_hours`, `reset_link_minutes`, `invitation_link_days`); they become realm settings.

**Access token claims:** `iss`, `sub`, `aud`, `client_id`, `exp`, `iat`, `jti`, `sid`, `auth_time`, `amr`, `acr`, `scope`, `pid_kind` (`user` or `service`), plus `email` and `name` when those scopes were granted. **No roles.**

**id_token claims:** `iss`, `sub`, `aud`, `exp`, `iat`, `auth_time`, `nonce`, `sid`, `amr`, `acr`, `at_hash`, `email`, `email_verified`, `name`, `locale`.

**Signing algorithm.** id_tokens and access tokens are signed with RS256. ES256 (shorter signatures) is available for a client that asks for it, and the libraries accept both. Three standards require RS256 (decision 10):
- OIDC Core section 15.1: an OpenID provider must support signing id_tokens with RS256. The only exception is for providers that issue unsigned id_tokens, which pacod-id never does.
- OIDC Discovery: `id_token_signing_alg_values_supported` in the discovery document must include RS256, and the conformance suite reads that document.
- RFC 9068 section 2.1: authorization servers and resource servers that use JWT access tokens must support RS256.

The gateway console's sealed cookie stays under 4 KB with an RS256 access token, because it stores no id_token (section [13.3](#133-console)).

**Refresh rotation**, taken from PACOD and moved from Redis into PostgreSQL:

- The `grants` row holds the current hash, the previous hash, the rotation time and the successor encrypted under a key derived from `PID_KEK` (wiped after 30 seconds). PACOD today keeps the plain successor in Redis (`routes_auth.py:614-623`); pacod-id does not keep it in plain text anywhere.
- A repeated refresh within 30 seconds receives the same successor, so two tabs or two requests refreshing at once both succeed.
- A superseded token presented after the grace window revokes **every SSO session and every grant of the account**, in every product and on every device, the Android app included. It emits `session.revoked` for each and sends a "we signed you out" security mail. This is PACOD's rule today (decision 9): a reused token means the token family may be in someone else's hands, so the owner signs in again everywhere.

### 5.3 Keys and JWKS

- Signing keys live in `signing_keys`. The private key is envelope-encrypted under `PID_KEK`, the scheme of the gateway's `crypto/envelope.rs`, with the key id bound in as associated data.
- One RS256 key (2048 bits) signs at a time. An ES256 key exists only while some client uses ES256.
- States: `next` (published, not yet used) → `active` → `retired` (still published for the longest token lifetime plus two days) → deleted.
- The job leader rotates automatically every 90 days. A new key is published at least 24 hours before it signs anything.
- Emergency rotation: `pacod-id keys rotate --now --revoke <kid>`.
- Libraries cache the JWKS for one hour and fetch it again on an unknown `kid`, at most once a minute.

### 5.4 SSO session, remember-me and devices

Cookies on the realm's host, all `HttpOnly`, `Secure`, `SameSite=Lax`:

| Cookie | Holds | Lifetime |
|---|---|---|
| `__Host-pid_sid` | the SSO session (opaque, stored as SHA-256) | session cookie; 7 days with "keep me signed in" |
| `__Host-pid_dev` | a known-device id, HMAC'd | 1 year |
| `__Host-pid_flow` | the sign-in transaction | 15 minutes |

- An SSO session lasts 12 hours, or 7 days with "keep me signed in" (decision 11). Both are realm settings; 7 days is the default in every realm. Its id is the `sid` claim.
- An Android sign-in lasts 7 days from its last use: each refresh renews it, so someone who opens the app at least once a week stays signed in, up to 365 days.
- A web grant ends at the earlier of its client's policy and its SSO session's end.
- A native grant is not tied to a browser session. It appears as its own device ("Vanga app on Pixel 8").
- The device list (`/account/sessions`) shows SSO sessions and native grants, with the products each one used. "Sign out" on a device revokes the session and its grants; "Sign out other devices" does it for all others. Both emit feed events.

### 5.5 Logout

- **RP-initiated logout** (`GET /oidc/logout`) ends the SSO session and every grant bound to it. On the PACOD platform, signing out of the gateway console also signs out of PACOD web in that browser, which is what single sign-on users expect. Without a valid `id_token_hint` and without an already-ended session, a confirmation page is shown first.
- **Server-to-server logout for BFFs.** A BFF that does not keep the id_token (the gateway console, section [13.3](#133-console)) ends the session with `POST /internal/v1/sessions/{sid}/revoke` and then sends the browser through `/oidc/logout?client_id=…&post_logout_redirect_uri=…`, which only clears the host cookie and redirects.
- **Back-channel logout.** pacod-id signs a `logout_token` (with `sid` and `sub`) and posts it to each affected client's `backchannel_logout_uri`, which may be an internal URL (`http://frontend:3000/auth/backchannel-logout`). Delivery goes through a PostgreSQL outbox and is retried for 10 minutes. It matters for BFFs with a server-side store (PACOD's Redis). A cookie-only BFF relies on the feed instead: the product rejects the revoked `sid` and the BFF clears its cookie on the 401.
- **No front-channel logout.**

### 5.6 The Android app

- AppAuth for Android opens a Custom Tab on `https://id.vanga.info/oidc/authorize`.
- The redirect URI is `https://vanga.info/native/oidc/callback`, a verified App Link (vanga.info already serves `assetlinks.json`). A custom scheme is used only as a fallback when verification fails.
- Google sign-in happens inside the Custom Tab, on pacod-id's page, not in the app.
- The refresh token is kept in Keystore-backed storage. API calls go to `/native/api/*` with the bearer token, as today.
- Sign-out calls `/oidc/revoke`.
- Sender-constrained tokens (DPoP) are left for a later phase.

### 5.7 Conformance

CI runs the OpenID Foundation conformance suite (in Docker) against a pacod-id container with these plans: Basic OP, Config OP, RP-Initiated Logout OP and Back-Channel Logout OP. Passing them is an exit criterion of phase 1; formal certification is optional.

---

## 6. Accounts

### 6.1 Passwords

- Argon2id with m=19456 KiB, t=2, p=1, which is what the gateway uses today (`Argon2::default()`) and the OWASP minimum. Configurable (`PID_ARGON2_M_KIB`); a hash with weaker parameters is replaced at the next successful sign-in.
- Policy: at most 128 characters, no composition rules, and a bundled offline list of the 100,000 most common passwords that are refused. The minimum length is a realm setting: **12 in the `pacod` and `gateway` realms, 8 in the `vanga` realm** (decision 1). It applies in the realm where the password is set. Existing shorter passwords keep working; the rule applies when a password is set.
- Because the account pool is shared, a password set on Vanga also opens PACOD and, on the PACOD platform, the gateway console. An account that holds any role beyond the implicit ones (for example `gw.org_admin` or `pacod.admin`) therefore needs 12 characters wherever it sets its password, and is asked to choose a longer one at its next sign-in when its current one is shorter (the flow answers `{next: "change_password"}`). Vanga's 8 characters thus apply to consumer accounts only.
- Unknown accounts are checked against a dummy hash, so they take as long as known ones (`mgmt/session.rs:157`), and every failure gives the same message.
- PACOD's bcrypt hashes are verified as they are and replaced with Argon2id at the next sign-in (section [12.4](#124-password-hashes)).

### 6.2 Email verification and password reset

The gateway's design is kept (`mgmt/account.rs`): 32 random bytes per link, stored as SHA-256, single use, bound to the address it was sent to, a new link replaces the old one, and a password change withdraws open reset links. A reset or a password change revokes all other sessions of the account and emits `session.revoked` for each.

Requests for links are answered the same way whether the address exists or not.

### 6.3 Sign-up

- On by default in the `pacod` and `vanga` realms. Per client, `self_signup` can turn it off; the gateway console's default is off, so a standalone gateway has no public sign-up.
- An address that already has an account gets the same "check your email" answer, and its owner receives an "you already have an account" mail. PACOD today answers 409 `email_taken` (`routes_auth.py:417`), which reveals who has an account.
- Consent to terms is recorded per realm and document version in `consents`.
- A self-registered account without gateway bindings that reaches the gateway console sees "you have no access yet; ask an administrator".

### 6.4 Invitations

Invitations move out of the gateway in their current form (`mgmt/members.rs`, migration `0005_invitations.sql`): the token is stored only as a hash, it is bound to the invited address, there is one pending invitation per address and scope, and an invitation that was emailed confirms the address when it is accepted. They become realm-aware: the mail and the link use the inviting product's brand.

Accepting creates the account (with a password or with Google) or, for someone who already has one, asks them to sign in to it first.

### 6.5 Google sign-in and linking

- One Google OAuth client per realm, so Google's consent screen shows the right brand.
- Authorization code with PKCE, `state` and `nonce`. The id_token is checked against Google's JWKS (`iss`, `aud`, `exp`, `nonce`). The account is keyed by Google's `sub`, not by the address. PACOD today reads the userinfo endpoint and does not check an id_token (`app/services/oauth_service.py`).
- Linking to an existing account by address requires Google's `email_verified=true` **and** either a recorded verification of the pacod-id account or fresh proof (its password, or an emailed link).
- In the `gateway` realm, Google creates a new account only when an invitation for that address is pending.
- An account always keeps at least one way to sign in; the last one cannot be unlinked.
- Tokens never pass through the browser's URL. PACOD today returns its own tokens in the URL fragment after Google sign-in (`/auth/oauth-success`, `/native/oauth-success`); with OIDC that path disappears.

### 6.6 Throttling and lockout

All counters are in PostgreSQL (`throttle`, updated with `INSERT … ON CONFLICT`), so every instance sees the same numbers. The gateway's limits today are per instance and in memory.

| Limit | Rule |
|---|---|
| Account | 10 failures in 15 minutes lock the account **for unknown devices only**. A browser whose `__Host-pid_dev` cookie has signed in to this account before keeps a separate limit of 10 in 15 minutes and is not locked out by others. |
| Address | 30 failures in 15 minutes per IPv4 address or IPv6 /64. |
| Mail | 3 requests in 15 minutes per purpose and address; 20 an hour per client address. Extra requests are dropped silently. |
| Service tokens | Products keep their per-address failure limit for bearer tokens (the gateway's 60 a minute). |

The known-device rule removes the weakness noted in `docs/threat-model.md:38`: "Anyone who knows an email address can lock that account for 15 minutes."

### 6.7 MFA and passkeys (later)

Phase 4 adds TOTP (secret encrypted under `PID_KEK`, hashed recovery codes) and WebAuthn passkeys (`webauthn-rs`), both as a second factor and passkeys also on their own. Tokens then carry real `amr` and `acr` values. Policies can require MFA per organisation and per role (for example `gw.platform_admin`), and products can ask for a fresh sign-in or a stronger one for sensitive pages through `max_age` and `acr_values`.

### 6.8 Organisation SSO (later)

Phase 5 lets an organisation sign in through its own identity provider: OIDC upstreams first (Entra ID, Google Workspace, Okta), domains proven by a DNS TXT record, the provider chosen from the address's domain, members created on first sign-in, then SCIM 2.0 provisioning. SAML only if a customer asks for it.

**A standalone gateway may also send its whole realm to the customer's provider** (decision 8):
- The `gateway` realm gets one upstream OIDC provider, configured in `PID_CONFIG`. The sign-in page goes straight to it.
- Accounts are linked by the upstream's `sub` and created at their first sign-in, without roles. Roles stay bindings in pacod-id, given in the console as today.
- Local passwords are switched off for everyone except a break-glass account (the bootstrap admin), which can still sign in at `/login?local=1`. A broken or misconfigured upstream therefore cannot lock the installation out.
- MFA is the upstream's job; its `amr` is passed on.
- Mapping the upstream's groups to roles, and SCIM, come with the organisation features.

### 6.9 Deleting an account

When an account is deleted, everything stored about it is deleted too, in pacod-id and in every product (decision 6).

- **Who:** the owner, from `/account`, after signing in again (`max_age`); or a holder of `id.admin`. An account that is the last holder of a protected role somewhere must hand it over first (section [7.3](#73-organisations-memberships-and-invitations)).
- **pacod-id** deletes the account and every row that belongs to it: SSO sessions, grants, federated identities, consents, verification and reset links, known devices, bindings, invitations sent to its address, pending mail to its address, throttle entries keyed by its address, and the identity audit log's rows about it. In audit rows of changes it made to other objects (another account, a binding), the actor is replaced by `deleted account`: the record of the change belongs to the other object and stays, the person's identity does not.
- **The feed** carries `principal.deleted {sub, external ids}`. Each product deletes its data for that account and confirms with `POST /internal/v1/deletions/{sub}/done?product=…`. Until every registered product has confirmed, pacod-id keeps only a tombstone (`account_deletions`: the id, the time, the products still pending), then removes it. A product that was offline longer than the feed's 7 days finds its pending deletions in the snapshot.
- **PACOD** deletes the shadow user and every row that refers to it. Seven tables follow through a cascading foreign key (`user_profiles`, `user_roles`, `device_sessions`, `user_activity` and others); about fourteen more carry a plain `user_id` column without one (`app/core/db_models.py`) and are deleted explicitly. A test lists every `user_id` column of the schema and fails when deletion does not cover one. PACOD also deletes the person's Open-WebUI account with its chats, and asks the gateway to erase what it holds under PACOD's user id.
- **The gateway** replaces the account with `deleted account` in its own audit log and in `template_versions.created_by`. Its usage events, audit-mode records (complete requests and responses) and end-user rollups in ClickHouse carry caller-supplied `user_id` and `end_user` values (`crates/pacod-gw-events/src/usage.rs:108`), which is where PACOD puts its user id. A new management endpoint, `POST /api/v1/orgs/{org}/workspaces/{ws}/erasure` with `{"user_id": …}` or `{"end_user": …}`, deletes them; PACOD calls it as part of a deletion, and any other caller can use it for its own users.
- **Backups and logs** are not rewritten. They age out within their retention periods, which the runbook states.

---

## 7. Authorization

### 7.1 Scopes, roles, permissions and bindings

- **Scope types.** `platform` (the installation, no id), `organisation` (an object in pacod-id), and types that products register with a parent type: `gw.workspace` (parent `organisation`) and `pacod.profile` (parent `platform`). PACOD's profiles and organisations are independent structures and stay so (decision 7).
- **Roles** are namespaced by product (`gw.org_admin`, `pacod.admin`) and declared in the product's manifest. Each role lists the scope types it may be bound at and the permissions it gives.
- **pacod-id's own roles:** `id.admin` (accounts, service accounts, clients, every binding) and `id.auditor` (reads the identity audit log).
- **Permissions apply downward.** A binding at a scope applies to that scope and everything below it, never above it. That is the gateway's rule today (platform admin rights reach every organisation and workspace).
- **A binding** is `{principal, role, scope}`. In the snapshot and the feed every binding carries its scope's full ancestor path, for example `[platform, organisation:o1, gw.workspace:w1]`. Evaluation is therefore a pure function of the bindings, and "can see this organisation at all" needs no extra lookup.

The engine has three questions:

| Function | True when |
|---|---|
| `allowed(permission, path)` | a binding of the principal sits on `path` or one of its ancestors and its role grants `permission` |
| `has_role(role, scope)` | the principal holds exactly this role at exactly this scope |
| `touches(scope)` | the principal holds any binding at `scope`, above it or below it |

### 7.2 Product manifests

Each product keeps `authz/<product>.yaml` in its own repository. It is compiled into the product's libraries and sent at start with `PUT /internal/v1/catalog/{product}`, which is idempotent. pacod-id refuses to drop a role that still has bindings unless the push says how to migrate them.

The gateway's manifest, written from `model/access.rs` and `mgmt/auth.rs`:

```yaml
product: gw
scope_types:
  workspace: { parent: organisation }
roles:
  platform_admin:  { at: [platform], permissions: ["gw.*"], protected: true }
  org_admin:       { at: [organisation], protected: true,
                     permissions: [org.manage, org.read, workspace.manage, workspace.read,
                                   audit_records.read, audit_log.read, members.manage] }
  workspace_admin: { at: [gw.workspace],
                     permissions: [workspace.manage, workspace.read, audit_records.read] }
  auditor:         { at: [platform, organisation, gw.workspace],
                     permissions: [org.read, workspace.read, audit_records.read, audit_log.read] }
  viewer:          { at: [platform, organisation, gw.workspace],
                     permissions: [org.read, workspace.read] }
grant:
  default: members.manage            # permission needed at the binding's scope to give or take a role
  platform_admin: platform.manage
```

Each check in `mgmt/auth.rs` maps onto one engine call: `can_admin_org(o)` is `allowed(org.manage, [platform, organisation:o])`, `can_read_audit_log(Some(o))` is `allowed(audit_log.read, [platform, organisation:o])`, `sees_org(o)` is `touches(organisation:o)`. The manifest also generates the role table in each product's documentation. That closes another drift: the gateway concept's section 5.4 says auditors and viewers are bound at an organisation or a workspace, while the code also allows the platform.

Members are managed by organisation admins only, for workspace members too (`mgmt/members.rs:223`, `:586`), so the manifest does not give `members.manage` to `workspace_admin`.

PACOD's manifest:

```yaml
product: pacod
scope_types:
  profile: { parent: platform }
roles:
  user:      { at: [platform], permissions: [chat.use, readings.use], implicit: true }
  admin:     { at: [platform], permissions: [admin.read, admin.write, profiles.assign], protected: true }
  superuser: { at: [platform], permissions: [admin.read, admin.write, profiles.assign, webui.admin,
                                             profiles.manage, prompts.edit, superuser.grant] }
  webui_service: { at: [platform], permissions: [chat.use, attribution.forward] }   # Open-WebUI's service account
custom_roles: { at: [platform] }    # created by admins at run time; no permissions yet
grant:
  default: admin.write
  superuser: superuser.grant
```

- `user` is implicit: every active account presenting a token for `pacod-api` holds it, so no row is stored for each sign-up.
- No role is bound at `pacod.profile` yet: which profiles a user may use stays PACOD's own data (`user_profiles`). The scope type is registered so that profile-level roles need no new mechanism later. Profiles do not belong to organisations (decision 7).
- The two `require_admin` functions become one `require_permission("pacod.admin.read")`, which admins and superusers both hold. Superuser-only routes require `pacod.profiles.manage` or `pacod.prompts.edit`. This is a deliberate change: a superuser without the admin role reaches the api's admin routes too, as it already reaches admin-api's.
- PACOD's escalation guard (`guard_superuser_role_change`) becomes the manifest's grant rule.
- **Custom roles stay, for future use** (decision 3). PACOD's admins create roles at run time today (`roles` and `user_roles`, `admin_api/routes/roles.py`), but its checks look only at `admin`, `superuser` and `user`, so custom roles grant nothing yet. In pacod-id:
  - the manifest allows them with `custom_roles: { at: [platform] }`;
  - admins create, rename and delete them through the admin API (`/admin/v1/custom-roles`); they are stored in `custom_roles` with a name and a description and are known as `pacod.custom.<name>`;
  - they are bound, carried in the feed and visible to `has_role` like any other role, but give no permission;
  - a later manifest version can give them permissions, or PACOD can check them with `has_role`, without moving any data;
  - the import carries PACOD's non-system roles and their assignments across.

### 7.3 Organisations, memberships and invitations

- **pacod-id owns an organisation's identity:** id, slug, display name, status, the products it uses, and later its verified domains and SSO settings.
- **Products own an organisation's configuration.** A product that creates an organisation registers it in pacod-id with its own id (section [7.8](#78-life-cycle-of-product-sub-resources)). The gateway's `org_…` ids stay.
- **Membership is derived:** a member of an organisation is anyone with a binding at it or below it. There is no separate membership table to keep in step.
- **Invitations** carry the role and scope to bind on acceptance (section [6.4](#64-invitations)).
- **The last-admin rule** (`mgmt/members.rs` today) is enforced by pacod-id for every role marked `protected`: removing a binding, deleting or disabling an account, deleting a service account, or declining a role may not leave a scope without one.

### 7.4 How products evaluate: snapshot and change feed

Three ways were possible:

| Way | Verdict |
|---|---|
| Roles in the access token | Rejected. A change would take effect only when the token expires, which is worse than the gateway's 1 second, and roles would have two sources of truth. |
| Ask pacod-id on every request | Rejected. It puts pacod-id on every management call, adds a network round trip and fails when pacod-id is down. |
| **A copy of the bindings in each product, kept current by a feed** | **Chosen.** It is the gateway's snapshot pattern applied to identity: lookups are in memory, and the data is small (explicit bindings and recent revocations; PACOD's `user` role is implicit). |

The feed:

- `GET /internal/v1/authz/snapshot?product=gw` returns the product's bindings and the `id.*` bindings, each with its path; the scope registry; disabled and deleted principals; the `sid`s revoked in the last 10 minutes; the catalogue version; and a sequence number `seq`.
- `GET /internal/v1/authz/changes?product=gw&after=<seq>&wait=25` is a long poll. pacod-id wakes waiting requests with PostgreSQL `LISTEN pid_authz`.
- Event kinds: `binding.added`, `binding.removed`, `principal.disabled`, `principal.enabled`, `principal.deleted` (section [6.9](#69-deleting-an-account)), `session.revoked {sid, sub}`, `credential.revoked {token_id, sub}`, `scope.upserted`, `scope.deleted`, `catalog.updated`, `account.updated {sub}`.
- **Ordering.** Every authorization write takes the single row of `authz_head` and increments its version, the design of the gateway's `config_head` (migration `0001_init.sql`). A follower can therefore never miss an event because two transactions committed out of order.
- Events are kept 7 days. A follower whose cursor is older gets 410 and loads the snapshot again.
- Products authenticate to the internal API with client credentials (`pacod-gw-core`, `pacod-api`).

What a product does per request:

1. Check the JWT: signature against the cached JWKS, `iss`, `aud`, `exp` (30 seconds leeway), `typ`.
2. Refuse when the `sid` is revoked or the `sub` is disabled.
3. Look up the bindings of `sub`.
4. Evaluate.

**Read your own writes.** After an admin changes a binding through a product's facade, the facade waits up to 2 seconds for its own cache to reach the `seq` pacod-id returned. Each gateway instance reports the `seq` it has applied in its `instances` heartbeat, so the gateway's `x-gw-applied: n/m` header keeps working for role changes.

### 7.5 Revocation targets and a stale feed

| Event | pacod-gw today | PACOD today | Target for both |
|---|---|---|---|
| Logout in the same browser | at once | at once (BFF session removed) | at once (BFF session removed, grant revoked) |
| Another device signed out, sessions ended by an admin | at once | up to 30 minutes | 2 seconds or less (p99) |
| Password changed or reset | at once | up to 30 minutes | 2 seconds or less, all other sessions revoked |
| Account disabled or deleted | at once | up to 30 minutes | 2 seconds or less |
| Role given or taken | about 1 second | up to 30 minutes | 2 seconds or less |
| Service or admin token revoked | about 1 second | – | 2 seconds or less (the feed clears the introspection cache) |

When the feed stops (`feed_age`, the time since the last successful poll, grows):

- **Admin surfaces** (the gateway's management API, PACOD's admin-api, PACOD's forward-auth endpoints) refuse with 503 `identity_unavailable` once `feed_age` passes 60 seconds (`PGW_ID_MAX_STALENESS`, `ID_MAX_STALENESS`).
- **Consumer surfaces** (PACOD's chat and readings, the Android app) keep using the last copy. The exposure is bounded by the 5-minute token lifetime, because refreshing needs pacod-id anyway.
- **The gateway's data plane** never consults identity and is unaffected.

### 7.6 One copy of the rules

- **Engine vectors** (`pacod-id/policy/engine-vectors/*.yaml`, with small test manifests) pin the engine's semantics: inheritance, scope matching, `touches`, implicit roles, grant rules and the last-admin rule. They run in `cargo test`, `pytest` and `vitest` in pacod-id's CI.
- **Product vectors** sit next to each manifest (`pacod-gw/authz/vectors/*.yaml`). They run in the product's own tests in every language the product uses: for the gateway the core's Rust tests and the console's vitest; for PACOD pytest and the frontend's vitest.
- The console's hand-written `ui/src/lib/access.ts` is replaced by helpers generated from the manifest.

A vector that fails against today's console:

```yaml
- name: an organisation admin reads that organisation's audit log
  bindings:
    - { role: org_admin, scope: "organisation:o1", path: [platform, "organisation:o1"] }
  checks:
    - { allowed: audit_log.read, at: [platform, "organisation:o1"], expect: true }   # access.ts says false
    - { allowed: audit_log.read, at: [platform], expect: false }
```

### 7.7 Machine principals

- **Service accounts** (`svc_…`) are principals with bindings, as people are. They belong to an organisation or to the platform.
- **Credentials:**
  1. **Static tokens `pidt_<id>_<secret>`** for scripts. Products check them with `POST /internal/v1/introspect`, cache the answer for 60 seconds keyed by the token's SHA-256, and drop it on `credential.revoked`.
  2. **Client credentials** for services that can run OAuth.
- **The gateway's `pgwa_` admin tokens keep working.** Scripts send a static bearer to the management API, and client credentials would break them. Existing admin tokens are imported as service accounts with their ids (`tok_…`); their HMAC is kept as a legacy hash, and pacod-id receives the gateway's pepper as `PID_LEGACY_PEPPERS` so it can verify `pgwa_` tokens on introspection. New tokens are `pidt_`. A legacy token works until it expires or is revoked.
- **Product-internal secrets stay in the product:** the gateway's `PGW_INTERNAL_TOKEN` (analytics to core jobs, not a principal), PACOD's `MCP_SHARED_SECRET` and `CRAWLER_INGEST_TOKEN`.
- **The gateway's data-plane keys (`pgw_…`) stay in the gateway.** They belong to a workspace, carry limits and are checked on the request path without any identity lookup.

### 7.8 Life cycle of product sub-resources

Today the gateway removes a workspace's or organisation's role bindings and invitations in the same transaction (`mgmt/resources/tenancy.rs:151`, `:227`). With bindings in pacod-id that is no longer one transaction, so it becomes a **transactional outbox**:

1. In the transaction that creates, renames or deletes the organisation or workspace, the gateway writes a row to a new table `idp_outbox` (operation, payload, attempts, next attempt, done).
2. After the commit it calls pacod-id, whose calls are idempotent: `PUT /internal/v1/scopes/gw.workspace/{id}` (parent, display name), `DELETE /internal/v1/scopes/gw.workspace/{id}`, `PUT /internal/v1/organisations/{id}`, `DELETE /internal/v1/organisations/{id}?product=gw`.
3. A leader task retries what is left.

- When a scope is deleted, pacod-id removes the bindings and invitations of the scope and everything below it and emits `binding.removed`.
- Between the commit and the delivery, bindings to a deleted scope grant nothing: the product builds paths only from its own configuration, and ids are never reused. Binding or inviting to a scope pacod-id does not know yet returns 409 `scope_unknown`.
- An organisation is deleted in pacod-id once no product has it registered.
- The gateway's `_system` organisation and workspace are registered at start as system scopes. They cannot be deleted.
- A configuration import into another gateway installation (`mgmt/transfer.rs`) registers the imported organisations and workspaces through the same outbox, with their ids.

---

## 8. Mail

- **One mail module** in pacod-id, with HTML and text layouts per brand (minijinja). The brand colours are generated from the design tokens, so the gateway's mail layout no longer copies the console's colours by hand (`mail.rs`, brand "PACOD AI Gateway" at line 17).
- **Strings come from the same message catalogues as the hosted UI** (`ui/messages/{en,bg,de,ru}.json`, keys starting with `mail_`).
- **Templates:** `verify_email`, `reset_password`, `invitation`, `password_changed`, `email_changed` (to the old address), `account_exists`, `new_sign_in` (new device), `session_revoked_reuse`.
- **Languages:** every mail carries Bulgarian and English together, Bulgarian first, as PACOD's mails do today (`app/services/email.py`): the subject reads `<Bulgarian> · <English> — <brand>`, and the body has a Bulgarian half followed by an English half. A brand may have a name per language ("Баба Ванга" and "Baba Vanga"). This holds in every realm (decision 2). The pair is a realm setting (`mail_locales`, default `bg, en`), so a standalone gateway abroad could change it without code; the account's language does not change it.
- **Brand for mail no user request started** (an admin sends a reset link): the account's `last_realm`, updated at each sign-in, and a required `realm` parameter on the admin endpoints that send mail. This replaces PACOD's guess from usage data (`admin_api/routes/users.py:310`, `_detect_product`).
- **Links** are always built from the realm's configured issuer URL, never from the Host header, as the gateway does today with `PGW_CONSOLE_URL`.
- **Delivery** goes through `mail_outbox`, sent by the job leader with `FOR UPDATE SKIP LOCKED`, retried with backoff. A sign-in flow never waits on SMTP.
- **Without SMTP** (a standalone gateway may have none), accounts need no verification, and invitation and reset links are shown to the admin who created them, as in the gateway today.
- Product mail (PACOD's newsletter, the gateway's alerts) stays in the products.

---

## 9. Audit log

pacod-id keeps its own `audit_log`:

- **Events:** sign-in succeeded or failed (with a reason category, never the password), lockout, logout, session revoked, refresh reuse detected, password and address changed, address verified, account created, disabled, enabled or deleted, Google linked or unlinked, binding added or removed, invitation created, accepted, declined or revoked, service token created or revoked, signing key rotated, client secret rotated.
- **Fields:** time, actor and actor kind, action, subject, organisation and scope where there is one, client, realm, client address, user agent, and a detail object.
- **Retention:** 400 days, removed by a daily job.
- **Access:** `id.admin` and `id.auditor` read everything. Products read the slice of one organisation through their facades, under their own permission (`audit_log.read` in the gateway).
- The gateway keeps writing its own audit row for each member change it passes through (with `config_version` empty), so an organisation's audit page in the console stays complete.

---

## 10. Hosted UI and the shared design package

### 10.1 Hosted sign-in and account UI

- SvelteKit with `adapter-static` (single-page mode), Svelte 5, Paraglide JS 2 for en, bg, de and ru, built from the `@pacod/ui` components.
- The Rust service serves `index.html` with `data-brand`, `data-theme` and `lang` filled in per request, so the right brand shows from the first paint. All calls go to the same host's `/ui-api/v1`.
- JavaScript is required, as at most identity providers. In return the page can have a strict Content-Security-Policy based on hashes, and `frame-ancestors 'none'`.

| Route | Purpose |
|---|---|
| `/login`, `/signup` | sign in, create an account (password or Google) |
| `/forgot-password`, `/reset-password` | ask for and use a reset link |
| `/verify-email` | confirm the address (the link only opens the page; the token is used when the page posts it, as in the gateway today) |
| `/invite` | look at and accept an invitation |
| `/logout` | confirm signing out |
| `/account` | name, address, language |
| `/account/security` | password; later MFA and passkeys |
| `/account/sessions` | devices and sessions |
| `/account/connections` | Google |
| `/account/organisations` | memberships and pending invitations |
| `/admin` | accounts, service accounts, clients, identity audit (`id.admin`, `id.auditor`), through `/ui-api/v1/admin/*` with the SSO cookie, since `/admin/v1` is internal only |

### 10.2 Tokens and themes

The design tokens live in `@pacod/ui` as JSON and are built into three CSS files:

| Output | For | Content |
|---|---|---|
| `tokens.css` | everyone | canonical custom properties `--pid-*`, light and dark through `light-dark()` and `color-scheme`, plus a `[data-theme]` override for an explicit choice |
| `console.css` | the gateway console | the gateway's current names mapped onto the canonical ones (`--bg: var(--pid-bg)`, `--surface: var(--pid-surface)` …), so `ui/src/app.css` keeps only layout rules |
| `tailwind.css` | PACOD | an `@theme inline` block mapping PACOD's names (`--color-surface`, `--color-surface-2` …) onto the canonical ones, so its utility classes keep working |

The two products name the same colours differently today (the gateway's `--surface` is PACOD's `--color-surface-2`, the gateway's `--bg` is PACOD's `--color-surface`), and the gateway raised the contrast of muted text in dark mode. The gateway's values become the source, since they were derived from PACOD's and then corrected for contrast.

**Brand themes** are `[data-brand="pacod" | "vanga" | "gateway"]`. They change the brand colour, the gold accent, the logo and the display font. PACOD's Vanga skin (`pacod-ui/frontend/src/lib/styles/fortune.css`) becomes the `vanga` theme. Fonts stay Manrope, Inter and JetBrains Mono; the font stacks name both the gateway's `'Inter Variable'` and PACOD's `'Inter'`.

### 10.3 Components

`AuthCard`, `Button`, `TextField`, `PasswordField` (show and hide, live length hint), `Checkbox`, `Notice`, `Divider` ("or"), `ProviderButton` (Google), `AccountMenu` (name, address, "Account settings" linking to the realm's `/account`, a slot for an organisation switcher, sign out), `BrandMark`, `LocaleSwitcher`, and in phase 4 `OtpInput`.

- Plain scoped CSS that reads only `--pid-*` variables, so a component looks the same in a plain-CSS app and in a Tailwind app.
- No icon package: icons are inline SVG or passed in as snippets. The gateway uses `@lucide/svelte`, PACOD `lucide-svelte`.
- No i18n library: every string is a property or a `messages` object. The gateway console is English only; PACOD uses Paraglide.
- Peer dependency `svelte ^5`. Nothing from SvelteKit.

### 10.4 SvelteKit 2 and 3

The gateway console runs SvelteKit 3 (Vite 8) and PACOD's frontend SvelteKit 2 (Vite 6). They also differ in how private environment variables are read (`$app/env/private` versus `$env/dynamic/private`).

- `@pacod/ui` uses no SvelteKit API at all.
- `@pacod/id-sveltekit` imports only SvelteKit **types**, never `$app/*` or `$env/*`. Each app passes its configuration in. Its peer dependency is `@sveltejs/kit ^2.20 || ^3`.
- CI builds two fixture apps, one on SvelteKit 2 and one on SvelteKit 3, and runs the sign-in flow end to end against a pacod-id container in both.
- Moving PACOD to SvelteKit 3 is not a prerequisite. It is best done after its cutover (phase 3), so a framework upgrade and an authentication change do not land together.

### 10.5 Languages

The hosted UI ships in English, Bulgarian, German and Russian, PACOD's four languages. Mails are in Bulgarian and English together (section [8](#8-mail)). The UI's language comes from `ui_locales`, then the account's setting, then `Accept-Language`, then the realm's default. The gateway console itself stays English; only its sign-in moves to the shared pages.

---

## 11. Client libraries

### 11.1 Rust: `pacod-id-client`

- `Verifier`: JWKS cache, RS256 and ES256, checks of `iss`, `aud`, `exp`, `typ`.
- `AuthzCache`: the snapshot plus a long-poll follower, held in an `ArcSwap` so lookups take no lock. Exposes `seq()` and `feed_age()`.
- `Introspector`: service tokens, cached 60 seconds, cleared by `credential.revoked`.
- `TokenExchanger`: the 60-second `aud=pacod-id` tokens for facades, cached per caller and session.
- `ScopeOutbox`: the SQL and the delivery task of section [7.8](#78-life-cycle-of-product-sub-resources).
- An optional axum extractor. The gateway keeps its own `Principal` extractor and calls the library from it.
- Dependencies: `jsonwebtoken` on the ring backend (the gateway already uses ring), `reqwest`, `arc-swap`, `pacod-id-authz`.

### 11.2 Python: `pacod_id`

- The same evaluator, `JwtVerifier` (PyJWT with `cryptography`, which PACOD already uses), and an `AuthzCache` that runs as an asyncio task started in the FastAPI lifespan.
- FastAPI dependencies `require_principal` and `require_permission(permission, path)`.
- Dependencies: PyJWT, httpx.

### 11.3 TypeScript and SvelteKit

| Package | Contents |
|---|---|
| `@pacod/id-authz` | the evaluator and a generator that turns a product manifest into typed helpers (`canAdminOrg(p, org)` …) |
| `@pacod/id-client` | framework-free: authorization code with PKCE, `state` and `nonce`, the RFC 9207 `iss` check, refresh behind a lock so only one request refreshes a session, the logout URL, verification of back-channel `logout_token`s. Built on `oauth4webapi` and `jose`. |
| `@pacod/id-sveltekit` | `createIdHandle(config)` returning a SvelteKit `Handle`, and route handlers for `/auth/login`, `/auth/callback`, `/auth/logout` and `/auth/backchannel-logout` |

### 11.4 Session stores

| Store | Used by | Notes |
|---|---|---|
| `sealedCookie` | the gateway console | AES-256-GCM sealed cookie under 4 KB: session id, `sub`, access token, refresh token, expiry times. No id_token (logout goes server to server, section [5.5](#55-logout)). No server state, so the console stays stateless. |
| `redis` | PACOD web, Vanga web | PACOD's existing `app_session:{id}` keys, plus an index `app_session_sid:{sid}` for back-channel logout and a `SET NX` lock for refresh. |
| `memory` | tests and development | |

---

## 12. Data

### 12.1 Schema

pacod-id has its own schema (`pacod_id`, in the gateway's database) or its own database (on the PACOD platform), always with its own database role, so no product's role can read password hashes. It connects directly or through a pool in session mode, because it uses `LISTEN` and advisory locks; PACOD's PgBouncer runs in transaction mode. Migrations use sqlx and only add, as the gateway's do. The tables, grouped (columns in [appendix A](#appendix-a-data-model)):

| Group | Tables |
|---|---|
| Accounts | `accounts`, `account_external_ids`, `federated_identities`, `consents`, `mfa_factors` (phase 4) |
| Machines | `service_accounts`, `service_tokens` |
| Authorization | `organisations`, `organisation_products`, `scopes`, `catalog`, `custom_roles`, `role_bindings`, `invitations`, `authz_head`, `authz_changes` |
| OIDC | `clients`, `auth_flows`, `auth_codes`, `sso_sessions`, `grants`, `signing_keys`, `backchannel_outbox` |
| Support | `account_tokens`, `throttle`, `known_devices`, `mail_outbox`, `audit_log`, `account_deletions`, `jobs` |

### 12.2 What leaves the gateway's configuration store

Users, role bindings and admin tokens are JSON documents in `config_objects` today and part of the configuration snapshot. Moving them out has these effects:

- Identity changes no longer raise the configuration version. Member and token endpoints answer with `x-pid-seq` and `x-gw-applied` (section [7.4](#74-how-products-evaluate-snapshot-and-change-feed)) instead of `x-gw-config-version`.
- The snapshot stops building users, user addresses, admin tokens and role bindings (`snapshot/build.rs`).
- Configuration export and import (`mgmt/transfer.rs`) already leave out users, admin tokens and role bindings; nothing changes there except the outbox registration of organisations and workspaces on import.
- The global settings `session_hours`, `remember_me_days` and `require_verified_email` (`model/settings.rs:146-151`) move to the `pacod-gw-console` client and the `gateway` realm. The default of "keep me signed in" drops from the gateway's 30 days to 7 (decision 11).
- The tables `sessions` and `account_tokens` go once the cutover is final; `invitations` moves to pacod-id.

### 12.3 PACOD's users become a local shadow

- **The id stays.** `users.id` is a foreign key across PACOD's tables. A column `idp_sub varchar(64) unique null` is added.
- **Just-in-time provisioning.** When a token's `sub` is unknown, PACOD inserts a shadow row (`INSERT … ON CONFLICT (idp_sub) DO NOTHING`) with a new id and assigns the active profiles, as `assign_all_active_profiles` does today. Workers keep a `sub → id` cache.
- **Columns that become copies of identity data:** `email`, the display name, `email_verified`, `is_active`, `last_login_at` and the terms columns. They are refreshed from token claims and on `account.updated` events.
- **Columns that stay PACOD's own:** the quota overrides and the profile assignments (`user_profiles`).
- **Deletion:** on `principal.deleted` the shadow row and everything that refers to it are deleted (section [6.9](#69-deleting-an-account)).
- **Moved into pacod-id:** `roles` and `user_roles` (system roles as `pacod.*` bindings, custom roles as `custom_roles` plus their bindings), `oauth_accounts`, `device_sessions`.
- **`password_hash`** is emptied one release after the import.
- **Nobody is signed out.** Device sessions are imported as legacy grants with their `refresh_token_hash` and `previous_token_hash`. Both systems store the SHA-256 of the raw token (PACOD as hex), so pacod-id's token endpoint accepts the old refresh tokens as they are, and BFF sessions and Android installs carry on. Two details make that work:
  - **Which client a legacy grant belongs to.** `device_sessions` does not record the site or app a session came from (`app/core/db_models.py:296-328`). A legacy grant is therefore accepted from any of `pacod-web`, `vanga-web` and `vanga-android`, and is bound to the client that presents it first. From then on it is an ordinary grant.
  - **Expiry.** An imported grant keeps the expiry PACOD gave it (up to 30 days). The new lifetimes (7 days, decision 11) apply from its next refresh, so nobody is signed out by the shorter default at the cutover.
  - **Sessions already in Redis.** Existing `app_session:*` entries hold a PACOD HS256 access token, which the new `AuthGuard` refuses. The new handle reads the old entry format, and when the stored access token was not issued by pacod-id it refreshes at once with the stored refresh token, then rewrites the entry in the new format.

### 12.4 Password hashes

- The gateway's Argon2id hashes are imported unchanged.
- PACOD's bcrypt hashes (`$2b$12$…`) are verified with the `bcrypt` crate and replaced with Argon2id after the next successful sign-in.
- The hash column holds the PHC string, so both kinds can sit side by side until every bcrypt hash is gone.

### 12.5 Merging accounts with the same address

This applies only on the PACOD platform. A standalone gateway has one source and never merges.

An address counts as **proven** on a side when:

| Side | Proven when |
|---|---|
| gateway | the user document has `email_verified: true` written explicitly |
| PACOD | `terms_accepted_at` is set and `email_verified` is true (the person registered and clicked the link), or a Google `oauth_accounts` row exists for that address |
| never proof | PACOD's server-side default `email_verified = true` for admin-created and Google-created users; the gateway's default when the field is missing (`model/access.rs:26`) |

| Case | Result |
|---|---|
| Proven on both sides | One account. The other side's id goes into `account_external_ids`. |
| Proven on one side | That side's credentials win; the other side's hash is not imported. The other side's product data links when the owner signs in with the merged account (PACOD matches by address only when `idp_sub` is empty and the account is verified). |
| Proven on neither side | The gateway's credentials win, and the account stays unverified until the address is confirmed. |

**Two passwords are never attached to one account.** Otherwise someone who registered a victim's address on PACOD without confirming it could sign in to the victim's gateway account. The import produces a dry-run report, which a person reviews before the import runs.

---

## 13. Integration: pacod-gw

### 13.1 Core

`mgmt/auth.rs`, `authenticate()`, accepts three kinds of bearer:

| Bearer | Handling |
|---|---|
| a JWT | `Verifier`, then `AuthzCache` (revoked `sid`, disabled `sub`, bindings) |
| `pidt_…` or `pgwa_…` | introspection, cached |
| `pgws_…` | refused once the gateway runs with `PGW_AUTH=oidc` |

- **`Principal` keeps its shape** (`id`, `kind`, `display`, `roles: Vec<(RoleName, RoleScope)>`). `session_hash` becomes `sid: Option<String>`. Bindings are mapped back: `gw.workspace:w` becomes `RoleScope::Workspace(w)` and the `gw.` prefix is dropped, so `RoleScope`'s JSON stays `{"type":"organisation","id":…}`. The methods (`is_platform_admin`, `can_admin_org` …) become thin wrappers over the generated manifest helpers, so the per-handler checks across `crud.rs`, `facade.rs`, `members.rs` and the others barely change.
- **`GET /api/v1/session` keeps its answer (`PrincipalView`) unchanged.** The tester (`tester/src/pgw_tester/auth.py`) keeps working: the console now forwards a JWT instead of a `pgws_` token. Its 30-second cache drops to 5 seconds, or it checks tokens itself with the Python package.
- **Other session endpoints:** `POST /session`, `PUT /session/password` and the reset and verification endpoints answer 410 `moved_to_identity` with the issuer URL. `DELETE /session` revokes the `sid` through the internal API. The invitation endpoints under `/session` answer 410 and the console sends people to pacod-id.
- **Removed from the core:** `crypto/password.rs`, `mail.rs`, `mgmt/account.rs`, the login and mail-request guards, the `pgws_` prefix, `ensure_first_admin` (`mgmt/mod.rs:332`), and the core's reading of `PGW_SMTP_*` and `PGW_EMAIL_*`, which it used for account mail only. The variables stay in compose: Alertmanager's renderer (`deploy/alertmanager/render.sh`) uses them, and compose passes the same values to pacod-id's `PID_SMTP_*`, so an operator sets mail once.
- **Added to the core:** the `idp_outbox` table and its delivery task, a `seq` column on the `instances` heartbeat, and the manifest push at start.
- **The first admin** comes from pacod-id's `PID_BOOTSTRAP_ADMIN_EMAIL`, `PID_BOOTSTRAP_ADMIN_PASSWORD` (or `_FILE`) and `PID_BOOTSTRAP_ADMIN_ROLES=id.admin,gw.platform_admin`. A binding to a product's role is held until that product's catalogue arrives. For one release the compose file maps `PGW_ADMIN_EMAIL` and `PGW_ADMIN_PASSWORD` onto the new names, and the core warns when they are still set.

### 13.2 Facades

The console keeps calling only the core (gateway concept section 3.2). The core's member, user and token endpoints become facades over pacod-id's admin API:

| Core endpoint | pacod-id |
|---|---|
| `/orgs/{org}/members`, `/orgs/{org}/workspaces/{ws}/members` | `/admin/v1/scopes/{type}/{id}/members` |
| `/orgs/{org}/invitations…` | `/admin/v1/invitations…` |
| `/role-bindings` | `/admin/v1/bindings` |
| `/admin-tokens` (create returns a `pidt_` token once) | `/admin/v1/service-accounts`, `/admin/v1/service-tokens` |
| `/users`, `/users/{id}/{verification,password-reset,email-confirmation}` | `/admin/v1/accounts…` |

- The core calls with a token obtained by token exchange from the caller's own token (section [5.1](#51-flows)), so pacod-id decides on the caller's own bindings, and names are merged in from the core's snapshot.
- **Trust assumption, stated plainly:** pacod-id trusts the core to present only tokens it received from callers. A compromised core can act for users who call it, within their own rights. It already has that power inside the gateway, and it cannot act for anyone who never called it.
- **Accounts on the shared platform.** Creating, disabling and resetting accounts needs `id.accounts.manage`. In the `gateway` realm a realm policy gives it to every holder of `gw.platform_admin`, so all of a standalone gateway's platform admins, including those promoted later, keep managing users as they do today; the first admin also has `id.admin`. On the PACOD platform a gateway platform admin does not manage PACOD's consumer accounts; gateway admins invite people instead (decision 4).

### 13.3 Console

- `hooks.server.ts` becomes `createIdHandle({ store: sealedCookie('__Host-pgw_session', PGW_UI_SESSION_SECRET), … })`. The rest of the hook stays: `locals.token` is the access token, `GET /session` fills `locals.principal`, and a 401 clears the cookie and goes to `/auth/login?next=…`.
- `login`, `forgot-password`, `reset-password`, `verify-email`, `invite` and `(app)/account/password` become redirects to pacod-id. `logout` becomes a POST to `/auth/logout`. Old invitation links (`/invite?token=…`) are forwarded to pacod-id, which still knows the token hashes.
- `ui/src/lib/access.ts` is generated from the manifest.
- `AuthCard.svelte` and the colour and type values in `ui/src/app.css` come from `@pacod/ui`.
- The account menu links to `${issuer}/account`.
- A self-registered account without gateway bindings sees a "no access yet" page instead of an empty console.

---

## 14. Integration: PACOD and Vanga

### 14.1 api and admin-api

- `AuthGuard.authenticate` (`app/guards/auth.py`) verifies the token with `pacod_id.JwtVerifier`, accepting the issuers of the `pacod` and `vanga` realms and `aud=pacod-api`; checks the cache for a revoked session or a disabled account; maps `sub` to the shadow user's id; and derives `roles` from the bindings plus the implicit `user`.
- `AuthenticatedUser` keeps its shape, so the existing `is_admin`, `is_superuser` and `require_*` call sites keep working. It gains `site`, taken from the `client_id` claim.
- The development token path (`dev-token-<id>`, behind `APP_ENV=development` and `AUTH_ALLOW_DEV_TOKENS=true`) stays for development only.
- `routes_auth.py` keeps `/auth/me` (PACOD's own view: profiles, quotas) and, for Android builds released before the cutover, an `/auth/refresh` shim that forwards to pacod-id's token endpoint until a minimum app version is enforced (`app/app_releases`). Everything else in it goes after the transition.
- admin-api's `users.py` changes identity fields through pacod-id's admin API with token exchange and keeps changing PACOD's own fields locally. `roles.py` becomes a facade over pacod-id's custom roles and `user_roles.py` over the bindings.

### 14.2 Frontend

- `handleAuth` in `hooks.server.ts` uses `@pacod/id-sveltekit` with the existing Redis store (section [11.4](#114-session-stores)). The 30-second `/auth/me` refresh stays, for product data.
- `(auth)/*` and `(fortune)/vanga/enter/*` are removed. "Sign in" and "Create account" go to `/auth/login` (with `prompt=create` and `ui_locales` from Paraglide). PACOD web uses the `pacod` realm, Vanga web on the fortune frontend the `vanga` realm with its own look.
- `/auth/oauth-success`, `/auth/set-session` and `/auth/callback` (the current Google return path) are removed.
- **forward_auth** endpoints (`/internal/monitoring-auth`, `/internal/webui-auth`) keep their code; their role data is now at most 2 seconds old instead of up to 30 minutes.
- **Open-WebUI (decision 5).** The bridge (`src/lib/server/webui-sso.ts`) depends only on the BFF session and stays in phase 3. Today it creates every Open-WebUI user with the role `user`; the only Open-WebUI admin is the bootstrapped `WEBUI_ADMIN_EMAIL` account. In phase 3b Open-WebUI becomes the OIDC client `pacod-webui`, and the bridge goes, with its derived passwords and its forged-token fallback:
  - Open-WebUI's own OIDC settings point at `https://id.pacod.io`: `OPENID_PROVIDER_URL`, `OAUTH_CLIENT_ID`, `OAUTH_CLIENT_SECRET`, `ENABLE_OAUTH_SIGNUP=true`, `OAUTH_MERGE_ACCOUNTS_BY_EMAIL=true` (so the accounts the bridge created are taken over, chats included), `ENABLE_LOGIN_FORM=false` and `ENABLE_PASSWORD_AUTH=false`. Because Open-WebUI merges accounts by address, pacod-id issues tokens to `pacod-webui` only for accounts whose address is verified; otherwise an unverified sign-up could take over someone's Open-WebUI chats.
  - **The one exception to "no roles in tokens":** for this client only, pacod-id adds a `roles` claim to the id_token and userinfo: `admin` when the account holds the permission `pacod.webui.admin`, otherwise `user`. Open-WebUI applies it at each sign-in (`ENABLE_OAUTH_ROLE_MANAGEMENT=true`, `OAUTH_ROLES_CLAIM=roles`, `OAUTH_ALLOWED_ROLES=user,admin`, `OAUTH_ADMIN_ROLES=admin`). The manifest gives `webui.admin` to `superuser` only, because Open-WebUI admins can usually read every user's chats (to verify for the version in use).
  - Open-WebUI keeps its own session after sign-in, so a role change reaches it at the next sign-in. That session is limited to 1 hour (`JWT_EXPIRES_IN=1h`), and the Caddy forward_auth gate stays in front of Open-WebUI as today: page loads still need a valid PACOD session, so a signed-out or disabled account is stopped within 2 seconds.
  - Open-WebUI calls PACOD's API today with a development token (`OPENAI_API_KEY: dev-token-webui_user` in `pacod-api/docker-compose.yml`), which PACOD accepts only in development mode. That token is replaced in phase 3, together with the switch of PACOD's guard, without waiting for phase 3b: Open-WebUI gets a service account with the role `webui_service` and a `pidt_` token, and forwards the person's identity in headers (`ENABLE_FORWARD_USER_INFO_HEADERS=true`) that PACOD's API trusts only from that service account, so usage stays attributed to the person.
  - The setting names are Open-WebUI's; the ones not yet checked against the version in use are `JWT_EXPIRES_IN`, `OAUTH_MERGE_ACCOUNTS_BY_EMAIL` and `ENABLE_FORWARD_USER_INFO_HEADERS` (to verify).

### 14.3 Vanga

- The `vanga` realm on `id.vanga.info`, with the Vanga theme, Vanga's Google OAuth client and the Vanga mail sender.
- Vanga web is the client `vanga-web` on the fortune frontend; the Android app is `vanga-android` (section [5.6](#56-the-android-app)). `/native/api/*` keeps working with the app's bearer; `/native/oauth-success` is retired.
- Accounts are the same as PACOD's, but a Vanga sign-in does not sign anyone in to PACOD (section [4.1](#41-realms-and-hosts)).
- Verification and reset mails move to pacod-id; the newsletter stays in PACOD.

---

## 15. Security

| Threat | Answer | What remains |
|---|---|---|
| Stolen access token | 5-minute lifetime, audience per product, `sid` revocable within 2 seconds | Valid until revoked or expired; DPoP later |
| Stolen refresh token | Rotation; on reuse every session and grant of the account is revoked and a security mail sent; the BFFs keep it on the server (Redis) or sealed (cookie) | A thief who refreshes first holds a session until the owner's next refresh. A client bug that replays a token signs its owner out everywhere |
| Login CSRF, mixed-up flows | `__Host-pid_flow` binds a flow to a browser; `state`, `nonce`, PKCE; `iss` in the response | – |
| Open redirects | Exact redirect URIs per client; `next` checked by the BFF (the console's `safeNext` stays) | – |
| Account enumeration | Same answers for known and unknown addresses (sign-in, sign-up, reset, verification); dummy hash | Timing of mail delivery |
| Password guessing | Argon2id, blocklist, account and address limits in PostgreSQL | Distributed guessing below the address limit; MFA in phase 4 |
| Lockout of someone else's account | Lockout applies to unknown devices only | A victim on a new device is locked out like everyone else |
| Theft of signing keys | Envelope-encrypted under `PID_KEK`, separate from the gateway's `PGW_KEK`; rotation and emergency revocation | A stolen KEK plus a database dump is enough |
| A compromised product | Its tokens' audience limits it to itself; token exchange works only with tokens its callers sent it; the internal API needs client credentials | It can act for its own callers within their rights |
| A forged feed | The internal listener is not public; products authenticate pacod-id by TLS or the private network and pacod-id authenticates products by client credentials | Inside the private network the transport is plain HTTP, as for the gateway's own internal calls |
| Wrong merges at migration | The proof rules of section [12.5](#125-merging-accounts-with-the-same-address), a reviewed dry run | – |
| Clickjacking and injected scripts | `frame-ancestors 'none'`, a strict CSP on the hosted UI | The gateway console still sends no CSP (`docs/threat-model.md:39`) |
| Secrets in logs | Caddy drops `code`, `state`, `token`, `id_token_hint` and `login_hint` from access logs, as it drops `token` today | – |
| Outbound requests | pacod-id calls only SMTP and Google's fixed endpoints | – |

---

## 16. Deployment and operations

### 16.1 Standalone gateway

**Compose** (`deploy/docker-compose.yml`):

- A new service `id`: image `ghcr.io/<org>/pacod-id` pinned by digest, networks `id-edge` (shared with Caddy alone, also its way out) and `backend`, `PID_*` environment, `depends_on` PostgreSQL being healthy, health check on `/readyz`, the same hardening anchor as the other services.
- **Database:** new installations get the role `pacod_id` and the schema `pacod_id` it owns in the gateway's database, created by an init script. Existing installations run a one-off `deploy/scripts/create-id-role.sh`. Running as the gateway's role with `search_path=pacod_id` is possible but documented as weaker, because the core's role could then read password hashes.
- **Caddy:** a new site `{$PGW_ID_HOST}` (`id.localhost` in development) proxying to `id:8070`, with a strict CSP and the log filter above. The console's site does not change.
- **Development ports**, in the gateway's 1xxxx range: public 18070, internal 18071.

**Helm** (`deploy/helm/pacod-gw`):

- A subchart `pacod-id`: a Deployment with two replicas, a PodDisruptionBudget, a Service with the ports `http-public`, `http-internal` and `metrics`, and an Ingress host `id.<domain>`.
- NetworkPolicy: `http-public` from the ingress controller; `http-internal` only from the core's admin pods and the console's pods.
- Values: `id.enabled`, `id.host`, `id.realm.{brand,locales,selfSignup}`, `id.google.*`, `id.databaseUrl`, and the new Secret keys.
- `id.enabled=false` with `id.issuer`, `id.internalUrl` and client credentials points the gateway at an external pacod-id. It must be pacod-id, not any OIDC provider, because of the feed.

### 16.2 PACOD platform

- One `pacod-id` service with two replicas on PACOD's internal network, using the database `pacod_id` on PACOD's PostgreSQL over a direct connection, not through PgBouncer.
- Caddy: `id.pacod.io`, and `id.vanga.info`, for which the manually installed vanga.info certificate must be reissued to cover the new host (or that host gets its own ACME certificate).
- The platform's gateway uses this instance (realm `pacod`, client `pacod-gw-console`), so the gateway console and PACOD web share single sign-on. The gateway's own `id` service is switched off there.
- Before phase 3, the host ports that `pacod-api/docker-compose.yml` publishes today are closed or bound to localhost: PostgreSQL (5432), admin-api (3001), Adminer (8081) and Qdrant (6333). The identity database will live on that PostgreSQL.

### 16.3 Network rules

- Public: only `/.well-known`, `/oidc/*`, `/federation/*`, the hosted UI and `/ui-api`.
- Internal listener (8071): product backends and BFFs only.
- Metrics (9170): Prometheus only.
- Outbound: SMTP, and Google (`accounts.google.com`, `oauth2.googleapis.com`, `www.googleapis.com`).

### 16.4 Secrets and keys

- `PID_KEK`, `PID_KEK_ID`, `PID_KEK_OLD`: the key-encryption key for signing keys, TOTP secrets and refresh successors. Separate from the gateway's `PGW_KEK`. Rotated with `pacod-id rewrap`, which, like the gateway's, refuses while a running instance has not confirmed the current key.
- `PID_TOKEN_PEPPER`: HMAC pepper for service tokens and known-device ids.
- `PID_LEGACY_PEPPERS`: the gateway's `PGW_KEY_PEPPER`, under the label `gw`, to verify imported `pgwa_` tokens.
- Client secrets for confidential clients, generated by `pacod-id clients secret <client>` and stored as hashes.
- Secrets can be read from files (`PID_SECRETS_DIR` and `<NAME>_FILE`), the gateway's convention.

### 16.5 Configuration reference

| Where | Variable | Meaning |
|---|---|---|
| pacod-id | `PID_DATABASE_URL` | PostgreSQL, direct or session-mode pool |
| pacod-id | `PID_CONFIG` | realms, clients, Google clients, policies (TOML) |
| pacod-id | `PID_PUBLIC_LISTEN`, `PID_INTERNAL_LISTEN`, `PID_METRICS_LISTEN` | listeners (8070, 8071, 9170) |
| pacod-id | `PID_TRUSTED_PROXIES` | addresses whose forwarding headers are believed |
| pacod-id | `PID_KEK`, `PID_KEK_ID`, `PID_KEK_OLD`, `PID_TOKEN_PEPPER`, `PID_LEGACY_PEPPERS` | section [16.4](#164-secrets-and-keys) |
| pacod-id | `PID_SMTP_HOST`, `PID_SMTP_PORT`, `PID_SMTP_TLS`, `PID_SMTP_USERNAME`, `PID_SMTP_PASSWORD`, `PID_EMAIL_FROM_ADDRESS`, `PID_EMAIL_FROM_NAME` | mail, overridable per realm |
| pacod-id | `PID_BOOTSTRAP_ADMIN_EMAIL`, `PID_BOOTSTRAP_ADMIN_PASSWORD`, `PID_BOOTSTRAP_ADMIN_ROLES` | first admin |
| pacod-id | `PID_ARGON2_M_KIB` | password hashing memory |
| gateway core | `PGW_AUTH` | `oidc` or `local` (rollback until `finalize`) |
| gateway core | `PGW_ID_ISSUER`, `PGW_ID_INTERNAL_URL`, `PGW_ID_CLIENT_ID`, `PGW_ID_CLIENT_SECRET`, `PGW_ID_AUDIENCE` | pacod-id connection; audience `pacod-gw` |
| gateway core | `PGW_ID_MAX_STALENESS` | feed age after which admin calls fail (60 s) |
| gateway compose | `PGW_ID_HOST` | host of the identity site in Caddy (`id.localhost` in development) |
| gateway console | `PGW_ID_ISSUER`, `PGW_ID_INTERNAL_URL`, `PGW_UI_CLIENT_ID`, `PGW_UI_CLIENT_SECRET`, `PGW_UI_SESSION_SECRET`, `PGW_CONSOLE_URL` | BFF sign-in |
| PACOD services | `ID_ISSUERS`, `ID_AUDIENCE`, `ID_INTERNAL_URL`, `ID_CLIENT_ID`, `ID_CLIENT_SECRET`, `ID_MAX_STALENESS` | token checks and feed; audience `pacod-api` |
| PACOD frontends | `ID_ISSUER`, `ID_CLIENT_ID`, `ID_CLIENT_SECRET` | BFF sign-in |

---

## 17. Migration

### 17.1 Gateway

1. `pacod-gw-core identity export` writes the raw `user`, `admin_token` and `role_binding` documents, the invitations, the organisations and workspaces, and the session settings to a file. A user document without an `email_verified` field is marked as not proven (section [12.5](#125-merging-accounts-with-the-same-address)).
2. `pacod-id import gw <file>` is idempotent and keeps ids: `usr_…` become account ids (`sub`), so the gateway's audit history stays linked; `gw.*` role bindings come along, and the `gateway` realm's policy lets every platform admin manage accounts (section [13.2](#132-facades)); `tok_…` become service accounts; `rb_…` and `inv_…` keep theirs. Argon2id hashes and invitation token hashes are copied as they are.
3. The core starts with `PGW_AUTH=oidc`. The snapshot ignores the identity documents but does not delete them. `PGW_AUTH=local` returns to the old behaviour, but identity changes made in pacod-id meanwhile are not copied back. Rollback is therefore allowed only while pacod-id has recorded no identity write for the gateway since the import: `pacod-gw-core identity rollback` compares the feed's sequence with the one the import returned and refuses otherwise.
4. `pacod-gw-core identity finalize` deletes those documents in one configuration version. A later migration drops `sessions` and `account_tokens`.

Console sessions end at step 3: everyone signs in once more. Admin tokens keep working throughout.

### 17.2 PACOD and Vanga

1. Add `users.idp_sub`.
2. `pacod-id import pacod --dry-run` reads PACOD's users, roles, Google links and device sessions, applies the merge rules against the accounts already imported from the gateway, and writes a report. A person reviews it.
3. The import runs: accounts (bcrypt hashes included), `pacod.*` bindings, custom roles and their assignments, federated identities, and device sessions as legacy grants. `idp_sub` is filled.
4. api and admin-api switch their guard; the frontends switch their BFF; Google's redirect URIs change to pacod-id's.
5. A new Android release with AppAuth ships; the refresh shim covers older builds until the minimum version is raised.
6. One release later: `password_hash` is emptied, the old auth routes and Redis keys go.

---

## 18. Phasing

| Phase | Content | Done when |
|---|---|---|
| **0. Design package and rules engine** | `@pacod/ui` with tokens, three themes and components; the engine in Rust, TS and Python; engine vectors; the gateway's manifest and vectors | Both apps use the tokens without visual change (screenshot comparison of the sign-in page and one app page, light and dark); both use the shared `AuthCard`; the console's `access.ts` is generated and the audit-log drift is fixed; vectors pass in all three languages |
| **1. pacod-id MVP** | Sections 4 to 12: service, hosted UI, libraries in beta, signed image, compose service and Helm subchart, threat model | The four conformance plans pass in CI; feed propagation is at most 1 second (p99) in an integration test; the token endpoint answers under 50 ms (p99, excluding Argon2); dry-run imports of copies of the gateway's and PACOD's data produce reviewed reports; an internal security review is done |
| **2. Gateway cutover** | OIDC mode in the core, facades, outbox, export and import, the console's BFF, compose and Helm, the gateway's docs (concept 5.4 and 19, runbook, threat model) | Console end-to-end tests and the tester pass on OIDC; upgrade from the previous release and rollback with `PGW_AUTH=local` are tested; revocation measured at 2 seconds or less; existing `pgwa_` scripts work unchanged; Helm lint and network-policy tests pass; signed release |
| **3. PACOD and Vanga cutover** | The shared instance, the guard, shadow users, import with merge, the BFFs of PACOD web and Vanga web, legacy grants, the Android release, Google moved, mails moved, the Vanga theme, Open-WebUI's service account instead of the development token. 3b: Open-WebUI through OIDC | No forced sign-outs (measured); role changes take 2 seconds or less; at least 95% of Android users on the new build before the shim goes; Redis holds no authentication state of PACOD's API; `password_hash` emptied; old auth routes gone; deleting an account is verified end to end across pacod-id, PACOD, Open-WebUI and the gateway; no development token is configured anywhere in production |
| **4. MFA and passkeys** | TOTP, passkeys, `amr`/`acr`, MFA policies, step-up | The gateway can require MFA for `gw.platform_admin`; step-up works on the credentials pages; conformance still passes |
| **5. Organisation SSO and SCIM** | OIDC upstreams per organisation, domain proof, provisioning; a realm-wide upstream for a standalone gateway | One external organisation signs in through Entra ID with members created on first sign-in; a standalone gateway signs in through a customer's provider and its break-glass account still works; SCIM passes Microsoft's validator |

Phase 0 is useful on its own: it unifies the look and removes the rules drift before any sign-in changes.

---

## 19. Risks and open decisions

### 19.1 Risks

1. **We run our own OIDC provider, and it guards both products.** Mitigation: a small feature set (first-party clients, code flow only), the conformance plans in CI, and an external review or penetration test before phase 3.
2. **pacod-id becomes the single place where sign-in can fail for every product.** Mitigation: two replicas, PostgreSQL high availability on the platform, the stale-feed rules of section [7.5](#75-revocation-targets-and-a-stale-feed); the gateway's data plane is unaffected.
3. **Wrong merges could hand an account to someone else.** Mitigation: the proof rules and a reviewed dry run.
4. **Only the OIDC half can be swapped.** The feed is pacod-id's own; moving to another provider would also need a replacement for the feed.
5. **Supporting SvelteKit 2 and 3 costs effort** until PACOD moves to 3.
6. **Android users update slowly**, which keeps the refresh shim alive.
7. **Parts outside our control:** the vanga.info certificate must be reissued, and each brand needs its own Google OAuth client.
8. **Feed volume.** Logouts are frequent on PACOD, so `session.revoked` events will be too. The 10-minute window in the snapshot and the 7-day retention keep it bounded; it is measured in phase 3.

### 19.2 Decisions

Paco decided every item on 2026-10-12; item 10 was a question of fact and is answered. The numbers stay, so references elsewhere in this document keep pointing at the right item.

1. **Decided:** Vanga's minimum password length is 8. Accounts with roles beyond the implicit ones still need 12 (section [6.1](#61-passwords)).
2. **Decided:** mails carry Bulgarian and English together, as PACOD's do today (section [8](#8-mail)).
3. **Decided:** PACOD's custom roles are kept for future use, without permissions for now (section [7.2](#72-product-manifests)).
4. **Decided:** on the PACOD platform, gateway platform admins do not disable or reset accounts; they invite (section [13.2](#132-facades)).
5. **Decided:** Open-WebUI signs in through OIDC, with a `roles` claim for that one client; only superusers are Open-WebUI admins; its session lasts at most 1 hour and the forward_auth gate stays. The development token it uses for PACOD's API is replaced by a service account in phase 3, whatever happens to Open-WebUI's sign-in (section [14.2](#142-frontend)).
6. **Decided:** deleting an account deletes everything stored about it, in pacod-id and in every product. Audit rows of changes the account made to other objects stay, with `deleted account` as the actor (section [6.9](#69-deleting-an-account)).
7. **Decided:** PACOD's profiles and organisations are independent structures (section [7.1](#71-scopes-roles-permissions-and-bindings)).
8. **Decided:** a standalone gateway may send its whole realm to the customer's identity provider, with a local break-glass account (section [6.8](#68-organisation-sso-later)).
9. **Decided:** refresh-token reuse signs the account out of every session, as PACOD does today (section [5.2](#52-tokens-and-lifetimes)).
10. **Answered:** ES256 alone is not allowed. OIDC Core, OIDC Discovery and RFC 9068 all require RS256, so pacod-id signs with RS256 and offers ES256 per client (section [5.2](#52-tokens-and-lifetimes)).
11. **Decided:** "keep me signed in" lasts 7 days by default in every realm; an Android sign-in lasts 7 days from its last use (section [5.4](#54-sso-session-remember-me-and-devices)).

These choices were made for this draft and can be overturned: the names `pacod-id`, `PID_` and `ID_`; where this document lives; no roles in tokens; a static hosted UI instead of a Node server; token exchange for facades.

---

## Appendix A. Data model

| Table | Key columns |
|---|---|
| `accounts` | `id` (`usr_…`), `email` (citext, unique), `email_verified_at`, `email_verified_source` (`verified`, `admin`, `federated`, `legacy_assumed`), `display_name`, `locale`, `status` (`active`, `disabled`, `deleted`), `password_hash` (PHC string), `password_changed_at`, `signup_realm`, `last_realm`, `created_at`, `last_login_at`, `deleted_at` |
| `account_external_ids` | `source` (`gw`, `pacod`), `external_id`, `account_id`; primary key (`source`, `external_id`) |
| `federated_identities` | `provider`, `provider_subject` (unique per provider), `account_id`, `email_at_link`, `created_at`, `last_used_at` |
| `consents` | `account_id`, `realm`, `document`, `version`, `accepted_at` |
| `service_accounts` | `id` (`svc_…` or imported `tok_…`), `name`, `organisation_id` (null for the platform), `status`, `created_by`, `created_at` |
| `service_tokens` | `id`, `service_account_id`, `token_hmac`, `legacy_hmac`, `legacy_pepper_id`, `hint` (last four characters), `expires_at`, `last_used_at`, `status` |
| `organisations` | `id` (`org_…`), `slug`, `display_name`, `status`, `created_at` |
| `organisation_products` | `organisation_id`, `product`, `registered_at` |
| `scopes` | `type`, `id`, `parent_type`, `parent_id`, `product`, `display_name`, `system`; primary key (`type`, `id`) |
| `catalog` | `product`, `version`, `manifest` (jsonb), `pushed_at` |
| `custom_roles` | `product`, `name`, `description`, `created_by`, `created_at`; primary key (`product`, `name`) |
| `role_bindings` | `id` (`rb_…`), `principal_id`, `principal_kind` (`user`, `service`), `role`, `scope_type`, `scope_id`, `source` (`manual`, `invitation`, `bootstrap`, `import`), `created_by`, `created_at`; unique (`principal_id`, `role`, `scope_type`, `scope_id`) |
| `invitations` | `id` (`inv_…`), `token_hash` (unique), `email`, `role`, `scope_type`, `scope_id`, `realm`, `invited_by`, `emailed`, `created_at`, `expires_at` |
| `authz_head` | one row: `version` |
| `authz_changes` | `version` (primary key), `at`, `kind`, `product`, `principal_id`, `payload` (jsonb) |
| `clients` | `id`, `realm`, `kind`, `secret_hash`, `redirect_uris`, `post_logout_redirect_uris`, `backchannel_logout_uri`, `audience`, `access_ttl`, `refresh_policy`, `self_signup`, `signing_alg`, `extra_claims` (only `pacod-webui`: `roles`) |
| `auth_flows` | `id`, `realm`, `client_id`, `request` (jsonb: redirect, state, nonce, PKCE challenge, prompt, locales), `browser_hash`, `account_id`, `stage`, `created_at`, `expires_at` |
| `auth_codes` | `code_hash`, `flow_id`, `account_id`, `sso_session_id`, `client_id`, `redirect_uri`, `pkce_challenge`, `nonce`, `scope`, `expires_at`, `used_at` |
| `sso_sessions` | `id` (= `sid`), `account_id`, `realm`, `cookie_hash`, `remember_me`, `auth_time`, `amr`, `acr`, `ip`, `user_agent`, `device_label`, `created_at`, `last_seen_at`, `expires_at`, `revoked_at`, `revoked_reason` |
| `grants` | `id`, `sso_session_id` (null for native), `client_id`, `account_id`, `scope`, `refresh_hash`, `previous_hash`, `rotated_at`, `successor_enc`, `successor_until`, `idle_expires_at`, `expires_at`, `revoked_at`, `legacy` |
| `signing_keys` | `kid`, `alg`, `state`, `public_jwk`, `private_enc`, `kek_id`, `created_at`, `activates_at`, `retires_at` |
| `backchannel_outbox` | `id`, `client_id`, `logout_token`, `attempts`, `next_attempt_at`, `done_at` |
| `account_tokens` | `token_hash`, `purpose` (`verify_email`, `reset_password`), `account_id`, `email`, `realm`, `created_at`, `expires_at` |
| `throttle` | `bucket`, `key`, `window_start`, `count`, `locked_until` |
| `known_devices` | `device_hash`, `account_id`, `first_seen_at`, `last_seen_at` |
| `mail_outbox` | `id`, `template`, `realm`, `locale`, `to`, `params` (jsonb), `attempts`, `next_attempt_at`, `sent_at`, `error` |
| `audit_log` | `id`, `at`, `actor`, `actor_kind`, `action`, `subject`, `organisation_id`, `scope_type`, `scope_id`, `client_id`, `realm`, `ip`, `user_agent`, `detail` (jsonb) |
| `account_deletions` | `account_id`, `requested_at`, `requested_by`, `pending_products` |
| `jobs` | `name`, `leader`, `lease_until`, `last_run_at` |

## Appendix B. Example calls

**Start sign-in** (the console's BFF redirects the browser):

```text
GET https://id.example.com/oidc/authorize?response_type=code&client_id=pacod-gw-console
    &redirect_uri=https%3A%2F%2Fconsole.example.com%2Fauth%2Fcallback
    &scope=openid%20email%20profile%20offline_access&state=…&nonce=…
    &code_challenge=…&code_challenge_method=S256
```

**Redeem the code** (BFF to pacod-id, inside the network):

```text
POST http://id:8070/oidc/token
Authorization: Basic base64(pacod-gw-console:<secret>)
grant_type=authorization_code&code=…&redirect_uri=…&code_verifier=…

200 {"access_token":"eyJ…","token_type":"Bearer","expires_in":300,
     "refresh_token":"pidr_…","id_token":"eyJ…","scope":"openid email profile offline_access"}
```

**Refresh twice within 30 seconds** (two requests of one session): both send `grant_type=refresh_token&refresh_token=pidr_A…` and both receive `pidr_B…`. A third use of `pidr_A…` after 30 seconds answers 400 `invalid_grant`, revokes every session and grant of the account, and emits `session.revoked` for each.

**Token exchange for a facade** (the core, acting for the caller):

```text
POST http://id:8070/oidc/token
Authorization: Basic base64(pacod-gw-core:<secret>)
grant_type=urn:ietf:params:oauth:grant-type:token-exchange
&subject_token=eyJ…(the caller's access token, aud=pacod-gw)
&subject_token_type=urn:ietf:params:oauth:token-type:access_token
&audience=pacod-id

200 {"access_token":"eyJ…(aud=pacod-id, act.sub=pacod-gw-core)","expires_in":60,…}
```

**Snapshot and changes** (the core's follower):

```text
GET http://id:8071/internal/v1/authz/snapshot?product=gw
200 {"seq":1842,"catalog_version":3,"bindings":[{"principal":"usr_…","role":"gw.org_admin",
     "path":["platform","organisation:org_…"]}, …],"disabled":["usr_…"],"revoked_sids":[…],"scopes":[…]}

GET http://id:8071/internal/v1/authz/changes?product=gw&after=1842&wait=25
200 {"seq":1843,"events":[{"seq":1843,"kind":"binding.removed","principal":"usr_…",
     "role":"gw.viewer","path":["platform","organisation:org_…","gw.workspace:ws_…"]}]}
```

**Introspect a service token:**

```text
POST http://id:8071/internal/v1/introspect
Authorization: Bearer <pacod-gw-core client-credentials token>
token=pgwa_…

200 {"active":true,"sub":"tok_…","pid_kind":"service","exp":null}
```

## Appendix C. Changes to the current code

**pacod-gw**

| File | Change |
|---|---|
| `crates/pacod-gw-core/src/mgmt/auth.rs` | `authenticate()` checks JWTs and introspects service tokens; `Principal` methods wrap the generated manifest helpers; `session_hash` becomes `sid` |
| `crates/pacod-gw-core/src/mgmt/session.rs` | `GET /session` stays; the other endpoints answer 410 or forward the revocation; lockout and dummy hash move to pacod-id |
| `crates/pacod-gw-core/src/mgmt/account.rs`, `src/mail.rs`, `src/crypto/password.rs` | removed |
| `crates/pacod-gw-core/src/mgmt/members.rs`, `mgmt/resources/access.rs` | facades over pacod-id's admin API |
| `crates/pacod-gw-core/src/mgmt/resources/tenancy.rs` | the binding and invitation cascade becomes an `idp_outbox` row |
| `crates/pacod-gw-core/src/mgmt/transfer.rs` | import registers organisations and workspaces through the outbox |
| `crates/pacod-gw-core/src/mgmt/facade.rs`, `crates/pacod-gw-analytics` | the erasure endpoint: deletes usage events, audit-mode records and end-user rollups by `user_id` or `end_user` |
| `crates/pacod-gw-core/src/mgmt/mod.rs` | `ensure_first_admin` removed; the manifest is pushed at start |
| `crates/pacod-gw-core/src/snapshot/build.rs` | identity documents no longer built |
| `crates/pacod-gw-core/src/model/settings.rs`, `mgmt/system.rs` | session settings removed |
| `crates/pacod-gw-core/src/bootstrap.rs` | `PGW_ID_*` read; the core no longer reads `PGW_SMTP_*`, `PGW_EMAIL_*`, `PGW_ADMIN_EMAIL` and `PGW_ADMIN_PASSWORD` (the SMTP values move on to pacod-id and Alertmanager) |
| `crates/pacod-gw-core/migrations/` | `idp_outbox`, a `seq` column on `instances`; later dropping `sessions` and `account_tokens` |
| `authz/gw.yaml`, `authz/vectors/` | new: the manifest and its vectors |
| `ui/src/hooks.server.ts`, `ui/src/lib/server/session.ts` | `createIdHandle` with the sealed-cookie store |
| `ui/src/routes/{login,forgot-password,reset-password,verify-email,invite}`, `ui/src/routes/(app)/account/password` | redirects to pacod-id |
| `ui/src/lib/access.ts` | generated from the manifest |
| `ui/src/app.css`, `ui/src/lib/components/AuthCard.svelte` | tokens and component from `@pacod/ui` |
| `tester/src/pgw_tester/auth.py` | shorter cache, or local checks with `pacod_id` |
| `deploy/docker-compose.yml`, `deploy/docker-compose.dev.yml`, `deploy/caddy/Caddyfile`, `deploy/helm/pacod-gw` | the `id` service, its site, its subchart and network policy |
| `docs/ai-gateway-concept.md` (5.4, 19), `docs/threat-model.md`, `docs/runbook.md` (13, 14), `docs/architecture.md` | updated in phase 2 |

**PACOD**

| File | Change |
|---|---|
| `pacod-api/app/guards/auth.py` | verification through `pacod_id`, roles from the cache |
| `pacod-api/app/api/dependencies.py`, `pacod-api/admin_api/dependencies.py` | one permission check instead of two `require_admin` |
| `pacod-api/app/api/routes_auth.py`, `app/services/{auth_service,oauth_service,email_tokens,session_config}.py` | removed after the transition, except `/auth/me` and the refresh shim |
| `pacod-api/app/services/email.py` | account mails removed |
| `pacod-api/app/services/` (new account deletion) | on `principal.deleted`: deletes everything under the user id, the Open-WebUI account, and calls the gateway's erasure endpoint; a test covers every `user_id` column |
| `pacod-api/app/core/db_models.py`, `app/main.py` (`_MIGRATIONS`) | `users.idp_sub`; later dropping `device_sessions`, `oauth_accounts`, `roles`, `user_roles` |
| `pacod-api/admin_api/routes/{users,roles,user_roles,oauth_accounts,session_config}.py` | facades over pacod-id or removed |
| `pacod-ui/frontend/src/hooks.server.ts`, `src/lib/server/{session,auth}.ts` | `@pacod/id-sveltekit` with the Redis store |
| `pacod-ui/frontend/src/routes/(auth)/*`, `src/routes/(fortune)/vanga/enter/*`, `src/routes/auth/{oauth-success,set-session,callback}` | removed |
| `pacod-ui/frontend/src/app.css`, `src/lib/styles/fortune.css` | tokens and the Vanga theme from `@pacod/ui` |
| `pacod-api/docker/caddy/Caddyfile`, `pacod-api/docker-compose.yml` | `id.pacod.io`, `id.vanga.info`, the `pacod-id` service; published database and admin ports closed; Open-WebUI's service token instead of the development token (phase 3) and its OIDC settings (phase 3b) |
| `pacod-ui/frontend/src/lib/server/webui-sso.ts`, `src/routes/auth/webui-launch` | removed in phase 3b |

## Appendix D. Manifest and vector format

**Manifest** (`authz/<product>.yaml`):

| Key | Meaning |
|---|---|
| `product` | short name, the prefix of roles, permissions and scope types (`gw`, `pacod`) |
| `scope_types.<name>.parent` | parent type: `platform`, `organisation` or another type of the same product |
| `roles.<name>.at` | scope types the role may be bound at |
| `roles.<name>.permissions` | permissions it gives; `gw.*` means all of the product's |
| `roles.<name>.protected` | the last binding at a scope cannot be removed |
| `roles.<name>.implicit` | every active account presenting a token for the product holds it at the platform; no row is stored |
| `custom_roles.at` | when present, admins may create roles at run time (`<product>.custom.<name>`), bindable at these scope types; they give no permission |
| `grant.default`, `grant.<role>` | the permission needed at the binding's scope to give or take a role |

**Vectors** (`authz/vectors/*.yaml`, `policy/engine-vectors/*.yaml`):

```yaml
manifest: ../gw.yaml            # engine vectors bring their own small manifest
cases:
  - name: a workspace admin cannot manage members
    bindings:
      - { role: workspace_admin, scope: "gw.workspace:w1",
          path: [platform, "organisation:o1", "gw.workspace:w1"] }
    checks:
      - { allowed: workspace.manage, at: [platform, "organisation:o1", "gw.workspace:w1"], expect: true }
      - { allowed: members.manage, at: [platform, "organisation:o1"], expect: false }
      - { touches: "organisation:o1", expect: true }
      - { has_role: [workspace_admin, "gw.workspace:w2"], expect: false }
```

Each case lists bindings and checks; a check is one of `allowed`, `has_role`, `touches`, `can_grant` or `last_admin`, with the expected answer. Every port loads the same files and must give the same answers.
