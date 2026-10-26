# pacod-id HTTP API

This is the reference of every HTTP endpoint pacod-id serves, as the code in
`crates/pacod-id/src` implements it. It is written for engineers of the products that sign
people in with pacod-id (their web apps, backends and native apps) and for operators. It
assumes OAuth 2.0 and OpenID Connect. The examples use the gateway, pacod-gw (product `gw`), whose
workspaces sit inside organisations.

## Contents

1. [Listeners](#1-listeners)
2. [Errors](#2-errors)
3. [OpenID Connect](#3-openid-connect)
4. [Internal API for products' services](#4-internal-api-for-products-services)
5. [Admin API](#5-admin-api)
6. [The hosted pages' API](#6-the-hosted-pages-api)
7. [Health and metrics](#7-health-and-metrics)

---

## 1. Listeners

pacod-id listens on three addresses (`http/mod.rs`).

| Listener | Default | Variable | Reachable from |
| --- | --- | --- | --- |
| Public | `0.0.0.0:8070` | `PID_PUBLIC_LISTEN` | browsers and apps, through the reverse proxy |
| Internal | `0.0.0.0:8071` | `PID_INTERNAL_LISTEN` | products' backends and BFFs only; in the pacod stacks `http://pacod-id-internal:8071` on `pacod-admin-net` |
| Metrics | `0.0.0.0:9170` | `PID_METRICS_LISTEN` | Prometheus and health checks |

Each variable takes an address or a host name with a port; a name is resolved when the listener
binds (runbook, section 2).

Which paths each listener serves:

| Path | Public | Internal | Metrics |
| --- | :-: | :-: | :-: |
| `GET /.well-known/openid-configuration` | yes | | |
| `GET /oidc/jwks` | yes | yes | |
| `GET, POST /oidc/authorize` | yes | | |
| `POST /oidc/token` | yes | yes | |
| `GET, POST /oidc/userinfo` | yes | yes | |
| `POST /oidc/revoke` | yes | yes | |
| `GET, POST /oidc/logout` | yes | | |
| `GET /federation/google/callback` | yes | | |
| `/ui-api/v1/*` (hosted pages, section 6) | yes | | |
| `/ui-api/v1/admin/*` (admin API with the session cookie, section 5) | yes | | |
| `/internal/v1/*` (section 4) | | yes | |
| `/admin/v1/*` (section 5) | | yes | |
| `GET /metrics`, `/healthz`, `/readyz` | | | yes |
| any other `GET` or `HEAD`: the hosted pages and their files | yes | | |

**The realm.** Endpoints a browser calls (discovery, authorize, logout, `/ui-api/v1`, the
pages) find their realm by the `Host` header, which must equal the authority of a realm's
issuer: its host, plus the port only when the issuer names one. An unknown host gets 404. The
token, revocation, userinfo and introspection endpoints do not look at `Host`: the token
endpoint takes the realm of the authenticated client, so a BFF can call
`http://id:8071/oidc/token` and still receive tokens whose `iss` is the realm's public URL.
Discovery is public only and needs the realm's host, so a product reads it from the public URL
even when it calls the token endpoint on the internal one.

**Common headers.** Every answer of the public and internal listeners carries
`X-Content-Type-Options: nosniff` and, unless the handler set its own, `Cache-Control:
no-store`. OAuth answers also carry `Pragma: no-cache`. Discovery (`max-age=300`) and the JWKS
(`max-age=3600`) are cacheable and send `Access-Control-Allow-Origin: *`; nothing else sends
CORS headers.

**Unknown paths.** On the internal listener, and on the public one for paths under `/ui-api/`,
`/oidc/`, `/federation/`, `/.well-known/`, `/internal/` and `/admin/v1/` or with a method other
than `GET` and `HEAD`, an unknown path answers 404 in the error shape of section 2.1. Other
public `GET` paths serve the hosted pages (`ui.rs`): a file of the build, or the page itself
with the realm's brand, language and theme filled in, under a hash-based
Content-Security-Policy. Without a build (`PID_UI_DIR`), pages answer 503 and the API works.

---

## 2. Errors

pacod-id answers errors in five shapes, depending on the endpoint.

### 2.1 Product API errors

The internal API, the admin API and `/ui-api/v1` use one shape (`error.rs`), so a
product's facade can pass an error on unchanged:

```json
{
  "error": {
    "type": "conflict",
    "message": "the change leaves organisation:org_1 without a gw.org_admin",
    "details": { "code": "last_holder", "role": "gw.org_admin", "scope": "organisation:org_1" }
  }
}
```

Every error carries `details.code`, the value to branch on: the specific code of the second
table below where the handler gives one, the `type` otherwise. So a plain 404 has the code
`not_found`, a missing or wrong internal-API credential `unauthorized`, the 403 of
`/internal/v1/sessions/{sid}/revoke` `forbidden`, and a gone sign-in flow `flow_expired`. Some
codes come with more fields in `details`. `message` is English text for people and may change.

| `type` | Status | Used for |
| --- | --- | --- |
| `bad_request` | 400; 413, 415 or 422 from axum | a malformed request, or one axum refuses before the handler (section 2.5) |
| `unauthorized` | 401 | no or an invalid credential, no session |
| `forbidden` | 403 | authenticated, not allowed |
| `not_found` | 404 | unknown, or not visible to the caller |
| `conflict` | 409 | the state does not allow the change |
| `validation_failed` | 422 | a field's value is refused |
| `rate_limited` | 429 | a throttle |
| `unavailable` | 503 | mail or Google not available |
| `internal` | 500 | anything unexpected (logged; the message says nothing more) |
| `scope_error` | 403, 409 or 422 | `PUT`/`DELETE` of organisations and scopes (section 4.4) |
| `gone` | 410 | the feed's cursor is too old (section 4.6) |
| `link_invalid` | 410 | a used, expired or replaced link |
| `flow_expired` | 410 | a sign-in flow that is gone or not this browser's |

The `details.code` values:

| Code | Status | Meaning |
| --- | --- | --- |
| `account_disabled` | 403, 409 | the account is disabled |
| `already_verified` | 409 | the address is confirmed already |
| `bad_deliver` | 422 | `deliver` is neither `email` nor `link` |
| `bad_email` | 422 | not an email address |
| `bad_expiry` | 422 | a service token's expiry is in the past |
| `bad_id` | 422 | an organisation or scope id is empty, too long or contains `:`, `/` or space |
| `bad_locale` | 422 | a language the realm does not offer |
| `bad_migration` | 422 | a catalogue push's `migrate` entry cannot be applied |
| `bad_name` | 422 | a service account or custom role name, or a manifest name, is refused |
| `bad_parent` | 422 | a scope's parent is not of the type the manifest names |
| `bad_request` | 400, 413, 415, 422 | a body or query string that does not parse (section 2.5), or a request a handler refuses as malformed |
| `bad_return_to` | 422 | `return_to` is not a path of the hosted pages, or contains control characters or white space |
| `bad_scope` | 422 | not `platform` or `type:id` |
| `bad_status` | 422 | `status` is neither `active` nor `disabled` |
| `binding_exists` | 409 | the principal holds this role at this scope already |
| `catalog_missing` | 409 | the product has not pushed its catalogue |
| `cross_origin` | 403 | a write to `/ui-api` without the realm's own `Origin` |
| `cursor_expired` | 410 | the feed no longer has the changes after `after` |
| `custom_roles_in_use` | 409 | a manifest drops custom roles while some exist |
| `custom_roles_off` | 422 | the product's manifest has no custom roles |
| `disabled` | 409 | a token for a disabled service account |
| `email_taken` | 409 | another account has the address |
| `email_unavailable` | 503 | the installation sends no mail |
| `google_off` | 404 | the realm has no Google sign-in |
| `google_unavailable` | 503 | Google's discovery cannot be read |
| `holds_more_rights` | 403 | through the realm policy, the account holds a role the actor could not grant (section 5.2) |
| `implicit_role` | 422 | an implicit role cannot be given |
| `invalid_credentials` | 401 | wrong address or password |
| `invalid_token` | 401 | the admin API's bearer is not a usable exchanged token |
| `invitation_exists` | 409 | another pending invitation for the address and scope that the actor may not replace; with `role` |
| `last_holder` | 409 | the change leaves a scope without a protected role's holder; with `role`, `scope` |
| `last_sign_in_method` | 409 | Google is the account's only way to sign in |
| `link_invalid` | 410 | a link (address confirmation, password reset, address change, invitation) is used, expired, revoked or replaced |
| `not_a_member` | 404 | the principal holds no role at the scope |
| `not_a_service` | 403 | the caller is not a product's service client |
| `not_allowed` | 403 | the actor's bindings do not allow it |
| `parent_changed` | 409 | a scope is registered under another parent |
| `password_too_short`, `password_too_long`, `password_too_common` | 422 | password policy; with `min_length` |
| `principal_unknown` | 404 | no account or service account with that id |
| `reauth_required` | 403 | the account page needs the password again (a sign-in in the last ten minutes) |
| `role_exists` | 409 | a custom role of that name exists |
| `role_in_use` | 409 | a manifest drops roles that bindings use; with `roles` |
| `same_email` | 422 | the new address is the current one |
| `scope_type_in_use` | 409 | a manifest drops scope types that registered scopes use; with `types` |
| `scope_type_not_allowed`, `unknown_role`, `bad_path` | 422 | the role cannot be held at that scope |
| `scope_unknown` | 409 | the scope (or a scope's parent) is not registered |
| `sign_in_required` | 401, 409 | no session, or the wrong account; sometimes with `email` |
| `signup_closed` | 403 | the realm or client does not offer sign-up |
| `slug_taken` | 409 | another organisation has the slug |
| `system_scope` | 409 | a system organisation or scope is not deleted |
| `terms_not_accepted` | 422 | the realm has terms and they were not accepted |
| `too_many_attempts` | 429 | a sign-in, sign-up or password throttle |
| `wrong_account` | 409 | the password is for another account than the one Google named |
| `wrong_product` | 403 | a service acting for another product, a manifest of another product, or the reserved product `id` |
| `wrong_stage` | 409 | the sign-in flow is not at this step |
| `bad_manifest`, `bad_product`, `unknown_parent`, `parent_cycle`, `unknown_scope_type`, `bad_permission`, `implicit_not_platform`, `unknown_grant_role`, `missing_grant_default` | 422 | a pushed manifest is invalid (`pacod-id-authz`, `manifest.rs`) |

### 2.2 OAuth errors

The token, revocation and introspection endpoints answer errors as RFC 6749 section 5.2
describes (`oidc/mod.rs`):

```json
{ "error": "invalid_grant", "error_description": "unknown, expired or revoked refresh token" }
```

Status 400, except `invalid_client` (401, with `WWW-Authenticate: Basic realm="pacod-id"` when
the client used HTTP Basic) and `server_error` (500). The values used: `invalid_request`,
`invalid_client`, `invalid_grant`, `unauthorized_client`, `unsupported_grant_type`,
`invalid_target`, `server_error`.

Userinfo answers 401 with `{"error": "invalid_request"}` and `WWW-Authenticate: Bearer
realm="pacod-id"` when no bearer is sent, 401 with `{"error": "invalid_token"}` and
`WWW-Authenticate: Bearer error="invalid_token"` when the token is not taken, and 500 with
`{"error": "server_error"}` when pacod-id fails on its side (the database, reading roles), so
that a client does not take its own failure for a signed-out person.

Introspection mixes two shapes: a missing or wrong caller credential is rejected before the
handler runs, as a 401 of section 2.1; a caller that is not a service gets 400
`unauthorized_client` in the OAuth shape.

### 2.3 Authorization redirects

Once `/oidc/authorize` trusts the redirect URI, an error goes back to it (RFC 6749 section
4.1.2.1) with `error`, `error_description`, `iss` (RFC 9207) and `state` when the request had
one. Section 3.3 lists the values.

### 2.4 HTML pages

Three browser endpoints answer with a short HTML page instead of JSON when they cannot send the
browser back: `/oidc/authorize` before the client and redirect URI are known to be good,
`/oidc/logout` when its parameters do not fit, and `/federation/google/callback` when the
Google sign-in fails. A form body or query string that axum cannot read is answered in JSON
instead (section 2.5).

### 2.5 Requests that do not parse

Bodies and query strings are read by axum's extractors. A JSON body that does not parse, has a
wrong type or an unknown field (most inputs refuse unknown fields), a missing
`Content-Type: application/json`, or a missing or malformed query parameter (for example
`after` on the feed) is rejected by axum before the handler runs. pacod-id turns axum's
plain-text answer into the shape of section 2.1 (`http/mod.rs`, `rejections_as_errors`): `type`
and `details.code` are `bad_request`, the status stays axum's (400, 413 for a body over its
limit, 415 or 422), and `message` is axum's text. A wrong type or an unknown field is therefore
422 `bad_request`, not `validation_failed`. This holds on the public and the internal listener,
also for the form posts of `/oidc/authorize` and `/oidc/logout` and the query of
`/federation/google/callback`.

---

## 3. OpenID Connect

### 3.1 Discovery

`GET /.well-known/openid-configuration` on the realm's host (`http/discovery.rs`). Every
endpoint is under the realm's issuer. The fields:

| Field | Value |
| --- | --- |
| `issuer` | the realm's issuer |
| `authorization_endpoint`, `token_endpoint`, `userinfo_endpoint` | `/oidc/authorize`, `/oidc/token`, `/oidc/userinfo` |
| `jwks_uri`, `end_session_endpoint`, `revocation_endpoint` | `/oidc/jwks`, `/oidc/logout`, `/oidc/revoke` |
| `scopes_supported` | `openid`, `email`, `profile`, `offline_access` |
| `response_types_supported`, `response_modes_supported` | `code`; `query` |
| `grant_types_supported` | `authorization_code`, `refresh_token`, `client_credentials`, `urn:ietf:params:oauth:grant-type:token-exchange` |
| `subject_types_supported` | `public` |
| `id_token_signing_alg_values_supported` | `RS256`, `ES256` |
| `token_endpoint_auth_methods_supported`, `revocation_endpoint_auth_methods_supported` | `client_secret_basic`, `client_secret_post`, `none` |
| `code_challenge_methods_supported` | `S256` |
| `claims_supported` | `iss`, `sub`, `aud`, `azp`, `exp`, `iat`, `auth_time`, `nonce`, `sid`, `amr`, `at_hash`, `email`, `email_verified`, `name`, `locale` |
| `prompt_values_supported` | `none`, `login`, `create` |
| `ui_locales_supported` | the realm's `locales` |
| `claims_parameter_supported`, `request_parameter_supported`, `request_uri_parameter_supported`, `require_request_uri_registration` | `false` |
| `authorization_response_iss_parameter_supported` | `true` |
| `backchannel_logout_supported`, `backchannel_logout_session_supported` | `true` |
| `frontchannel_logout_supported` | `false` |

No introspection endpoint is advertised: introspection is internal (section 3.8).

### 3.2 JWKS and signing keys

`GET /oidc/jwks` returns every published key: the active key per algorithm, the next key
(published 24 hours before it signs) and retired keys (kept 49 hours after they stop signing).
It is the same document for every realm. Keys rotate every 90 days (`keys.rs`); RS256 always has
a key, ES256 only once a client is configured with it. Every instance reloads the keys every 30
seconds. Cache the JWKS for at most an hour and fetch it again on an unknown `kid`.

### 3.3 Authorization endpoint

`GET` or `POST` (form) `/oidc/authorize`, on the realm's host (`oidc/authorize.rs`).
Only the authorization code flow, with PKCE S256 for every client, confidential and public
alike.

The checks come in this order. Until the redirect URI is trusted, a failure is an HTML page:

1. A parameter given twice: 400 page.
2. No realm for the `Host`: 404 page.
3. `client_id` unknown: 400 page. The client belongs to another realm, is a service client, or
   lacks the `authorization_code` grant: 400 page.
4. `redirect_uri` not exactly one of the client's `redirect_uris`: 400 page.

After that, errors redirect to the client (section 2.3):

| Condition | `error` |
| --- | --- |
| `request` given | `request_not_supported` |
| `request_uri` given | `request_uri_not_supported` |
| `response_type` is not `code` | `unsupported_response_type` |
| `response_mode` given and not `query` | `invalid_request` |
| `scope` lacks `openid` | `invalid_scope` |
| no `code_challenge`, `code_challenge_method` not `S256`, or a challenge that is not 43 base64url characters | `invalid_request` |
| an unknown `prompt` value, or `none` with another value | `invalid_request` |
| `id_token_hint` that is not an id_token pacod-id signed for this realm | `invalid_request` |
| `max_age` not a non-negative integer | `invalid_request` |
| `prompt=none` and no usable session (also when the session's account is not the one `id_token_hint` names) | `login_required` |
| `prompt=none` and the account must choose a new password first | `interaction_required` |
| the client has a `roles_claim` and the account's address is not confirmed | `access_denied` |

Parameters:

| Parameter | Handling |
| --- | --- |
| `client_id`, `redirect_uri`, `response_type` | required, as above |
| `scope` | must contain `openid`. Other values are not checked: they are kept, space-normalised, and appear in the tokens' `scope`. `email` and `profile` add claims. `offline_access` changes nothing: a client with the `refresh_token` grant always receives a refresh token. |
| `state` | returned with the code or the error |
| `nonce` | put into the id_token |
| `code_challenge`, `code_challenge_method` | required; `S256` only |
| `prompt` | `none`, `login`, `create`, `consent`, `select_account`. `login` and `create` always start a new sign-in; `create` opens the sign-up page; `consent` and `select_account` are accepted and have no effect (there is no consent screen) |
| `max_age` | seconds; a session older than this is not used, and `0` always asks for a new sign-in |
| `login_hint` | passed to the hosted page and on to Google |
| `ui_locales` | passed to the hosted page |
| `id_token_hint` | who the client expects: an id_token pacod-id signed for this realm (`typ` `JWT`, `iss` the realm's issuer), checked as at logout (section 3.9; an expired one is accepted, one whose key has left the JWKS is not, its `aud` is not compared with `client_id`). A session of another account than its `sub` is not used. |
| `acr_values`, `claims`, `display` and anything else | ignored |

**Single sign-on.** When the browser sends a live session cookie (`pid_sid`) of this realm
whose account is active (and, with an `id_token_hint`, is the account it names), the session
satisfies `max_age`, and `prompt` has neither `login` nor `create`, a code is issued at once:
`303` to `redirect_uri` with `code`, `iss` and `state`. The code lives 60 seconds and works
once.

**Otherwise a sign-in flow starts.** pacod-id stores the request as a flow (15 minutes), sets
the `pid_flow` cookie that binds the flow to this browser, and answers `303` to
`{issuer}/login?flow=<id>` (`/signup?flow=<id>` with `prompt=create`). The hosted pages drive
the flow through `/ui-api/v1/flows/{id}` (section 6); at its end the browser is sent to the
client with the code. A session whose account must choose a new password starts its flow at
that step only when it signed in within the last 10 minutes (its `auth_time`); with an older
session the person signs in again, and the password step leads to the change. A Google sign-in
of an account that must choose a new password leads to that step too. Each completed sign-in
creates a new SSO session; it never upgrades the old one.

**Cookies** (`oidc/mod.rs`). `pid_sid` (the SSO session), `pid_flow` (15 minutes) and `pid_dev`
(a known-device id, one year) are `HttpOnly`, `SameSite=Lax`, `Path=/`. When the issuer is
`https`, they are `Secure` and named with the `__Host-` prefix (`__Host-pid_sid`); over plain
`http` in development they are not. `pid_sid` is a browser-session cookie, or lasts
`remember_me_days` with "keep me signed in".

### 3.4 Token endpoint

`POST /oidc/token`, on the public and the internal listener (`oidc/token.rs`). The body must be
`application/x-www-form-urlencoded`; a parameter given twice is `invalid_request`. Every answer
has `Cache-Control: no-store`.

**Client authentication** (`authenticate_client`):

| Method | How |
| --- | --- |
| `client_secret_basic` | `Authorization: Basic` with the form-urlencoded `client_id:secret` (RFC 6749 section 2.3.1). A `client_id` in the body must then match. |
| `client_secret_post` | `client_id` and `client_secret` in the body |
| `none` | a public client sends `client_id` alone |

Confidential and service clients must send their secret; a public client must not. Both
methods at once is `invalid_request`. Any failure is 401 `invalid_client`. Secrets come from
the variable a client's `secret_env` names, or from `pacod-id clients secret <client>`, and are
stored as HMACs under `PID_TOKEN_PEPPER`.

**Grants.** A client may use the grants its configuration lists; without a list, confidential
and public clients have `authorization_code` and `refresh_token`, service clients
`client_credentials` and token exchange. A known grant the client may not use is
`unauthorized_client`, an unknown one `unsupported_grant_type`. The tokens' issuer is the
client's realm.

#### authorization_code

Parameters `code`, `redirect_uri` and `code_verifier`, all required.

- The code must be this client's, unexpired, sent with the same `redirect_uri`, and the
  verifier (43 to 128 unreserved characters) must hash to the challenge. Otherwise
  `invalid_grant`.
- A code used a second time is `invalid_grant`, and the refresh token the first use produced is
  revoked (audit `code_replayed`). Access tokens already issued stay valid until they expire.
- A code presented by another client is `invalid_grant` like an unknown one: it neither
  spends the code nor revokes anything, and the client it was issued to can still redeem it.
- The account must be active and, when the code came from a session, the session live.

Answer:

```json
{ "access_token": "eyJ…", "token_type": "Bearer", "expires_in": 300,
  "id_token": "eyJ…", "scope": "openid email profile", "refresh_token": "pidr_…" }
```

`expires_in` is the client's `access_ttl_seconds` (120 to 900, default 300). `refresh_token` is
present when the client has the `refresh_token` grant.

#### refresh_token

Parameter `refresh_token`. Allowed to clients with the `refresh_token` grant, and to service
clients with token exchange (they refresh their delegated grants).

- **Rotation.** Every refresh returns a new refresh token. Presenting the token that was just
  replaced, within 30 seconds of its rotation, returns the same successor again, so two tabs
  refreshing at once both succeed.
- **Reuse detection.** A superseded token of the grant (the previous one after the 30 seconds,
  or an older one, kept as hashes in the table `refresh_spent`) ends every SSO session and
  every grant of the account, emits `session.revoked` for each, queues back-channel logouts,
  mails the owner (`session_revoked_reuse`) and answers `invalid_grant`. A `pidr_` token
  naming the grant with a secret the grant never had
  is only `invalid_grant`: it signs nobody out, since the grant's id alone is not secret. The
  sealed successor is kept for its 30 seconds only; the maintenance job blanks it afterwards.
- **Lifetimes.** A confidential client's grant ends with the SSO session it was issued in
  (`session_hours`, default 12, or `remember_me_days`, default 7) and fails once that session
  has ended. A public client's grant is not bound to the session's expiry: it lasts
  `refresh_idle_days` (default 7) from its last use, at most `refresh_absolute_days` (default
  365) in all. Every grant carries the `sid` it was issued in and is revoked when that session
  is ended (logout, sign-out elsewhere, password change).
- An imported grant can be claimed by any client of a list; the first refresh makes it that
  client's.

Answer: `access_token`, `token_type`, `expires_in`, `refresh_token`, `scope`, and an `id_token`
when the scope has `openid` and the grant is not delegated (without `nonce`).

#### client_credentials

For service clients. No parameters. The answer is `access_token`, `token_type` and
`expires_in`; no refresh token, no `scope`. The token is the bearer for the internal API
(section 4.1). Its claims: `iss`, `sub` and `client_id` (the client), `aud` `"pacod-id"`,
`iat`, `exp`, `jti`, `pid_kind` `"service"`.

#### Token exchange (RFC 8693)

`grant_type=urn:ietf:params:oauth:grant-type:token-exchange`, for service clients
(`oidc/exchange.rs`).

| Parameter | Handling |
| --- | --- |
| `subject_token` | required: a user access token or a service token, see below |
| `subject_token_type` | required: `urn:ietf:params:oauth:token-type:access_token` |
| `requested_token_type` | `urn:ietf:params:oauth:token-type:access_token` (default) or `urn:ietf:params:oauth:token-type:refresh_token` |
| `audience` | default `pacod-id`; for an access token it must be `pacod-id`; ignored for a refresh token |
| `actor_token` | refused (`invalid_request`): the authenticated client is the actor |

The subject token is either a service token (`pidt_…`, or an imported `pgwa_…` when
`PID_LEGACY_PEPPERS` has the `gw` pepper) that is active, unexpired and whose service account
is active; or a user access token pacod-id issued: signature, `typ` `at+jwt`, the issuer of any
configured realm, unexpired, `pid_kind` `user`, its session live and its account active, and
its `aud` the service client's `audience` (or a list containing it): only tokens issued for the
client's own product are taken. Every service client that may exchange tokens must name an
`audience` in the configuration (docs/runbook.md section 3). Service tokens are not bound to an
audience. Anything else is `invalid_grant`.

| Requested | Subject | Result |
| --- | --- | --- |
| access token, `audience=pacod-id` | user | a 60-second user access token with `aud` `pacod-id` and `act: {"sub": <service client>}`; `sid`, `scope`, `auth_time` and `amr` come from the subject token |
| access token, `audience=pacod-id` | service token | a 60-second token: `iss`, `sub` (the service account), `aud` `pacod-id`, `client_id`, `iat`, `exp`, `jti`, `act.sub`, `pid_kind` `service`, `name` |
| refresh token | user | a **delegated grant**: a refresh token bound to the person's session, ending with it and after 12 hours at the latest, plus an access token with `aud` the service client's `audience`, `act.sub` the service client, lifetime `access_ttl_seconds`. The service refreshes it with `grant_type=refresh_token`, with rotation as above. |
| refresh token | service token | `invalid_request` |
| anything else | | `invalid_target` |

Answer: `access_token`, `issued_token_type` (always the access-token type, also when a refresh
token was requested), `token_type` `Bearer`, and `refresh_token` for a delegated grant. There is
no `expires_in`.

### 3.5 Tokens

**Access tokens** are JWTs (RFC 9068): header `typ` `at+jwt`, `alg` RS256 or ES256 (the client's
`access_token_alg`), `kid`. A user access token carries:

| Claim | Value |
| --- | --- |
| `iss` | the realm's issuer |
| `sub` | the account id (`usr_…`), the same in every realm |
| `aud` | a string: the client's `audience` (its product), or `pacod-id` for an exchanged token |
| `client_id` | the client the token was issued to |
| `iat`, `exp`, `jti` | always |
| `sid` | the SSO session |
| `auth_time`, `amr` | of the sign-in; `amr` is `["pwd"]` or `["fed"]` (Google) |
| `scope` | the granted scope string |
| `pid_kind` | `user` |
| `email` | with the `email` scope |
| `name` | with the `profile` scope, when the account has a display name |
| `act` | `{"sub": <client>}` on exchanged and delegated tokens |

There are no roles and no `acr`. Products evaluate roles from the feed (section 4.6). Service
tokens (client credentials and exchanged service tokens) are listed in section 3.4.

**The id_token** has header `typ` `JWT` and the client's `id_token_alg`, and lives 5 minutes:
`iss`, `sub`, `aud` and `azp` (the client), `iat`, `exp`, `auth_time`, `sid`, `amr`, `at_hash`,
`nonce` (when the authorization request had one), `email` and `email_verified` (scope `email`),
`name` and `locale` (scope `profile`; each only when set), and `roles` for a client configured
with a `roles_claim`: each configured value whose permission the account holds anywhere, else
the configured default.

**Refresh tokens** are opaque, `pidr_<grant id>_<43 base62>`, stored as SHA-256. **Service
tokens** are `pidt_<id>_<43 base62>`, stored as HMACs under `PID_TOKEN_PEPPER`; the admin API
creates them (section 5.4).

### 3.6 Userinfo

`GET` or `POST /oidc/userinfo` with `Authorization: Bearer <access token>`, on both listeners.
Any user access token pacod-id issued is accepted, whatever its audience, while its session is
live and its account active. The answer: `sub`; `email` and `email_verified` with the `email`
scope; `name` and `locale` (each when set) with the `profile` scope; and `roles` when the token's
`client_id` names a client with a `roles_claim`. Errors in section 2.2; a failure on pacod-id's
side is 500 `server_error`, not `invalid_token`.

### 3.7 Revocation

`POST /oidc/revoke` (RFC 7009), form-encoded, on both listeners, with client authentication as
at the token endpoint. Parameter `token`; `token_type_hint` is ignored. When `token` is the
current refresh token of a grant of the calling client, the grant is revoked. The answer is
always 200 with an empty body, whether or not anything was revoked. Access tokens cannot be
revoked: they are JWTs and live until `exp`. Revoking a grant does not end the SSO session,
emits no feed event and sends no back-channel logout; to end a session use logout (section
3.9).

### 3.8 Introspection

`POST /internal/v1/introspect` (RFC 7662), internal listener only, for products' service clients
(authenticated as in section 4.1). Form parameter `token`.

| Token | Answer |
| --- | --- |
| a service token: `pidt_…`, or `pgwa_…` checked with the `gw` pepper of `PID_LEGACY_PEPPERS` | `{"active": true, "sub": <service account>, "pid_kind": "service", "token_id", "name", "organisation_id", "exp"}`; `exp` is null for a token without expiry |
| a user access token pacod-id signed, unexpired, session live, account active | `{"active": true, "sub", "pid_kind": "user", "client_id", "aud", "scope", "sid", "iss", "iat", "exp", "token_type": "Bearer"}` |
| a service JWT pacod-id signed (`pid_kind` `service`), unexpired, whose `sub` is an active service account (an exchanged service token, section 3.4) or a configured client (a client-credentials token) | the same fields as the line above |
| anything else | `{"active": false}`, without a reason |

A service token is inactive once it is revoked or expired, or its service account is disabled
or deleted. A service JWT exchanged from one is active until its `exp` (60 seconds), also
after the token it came from is revoked, unless its service account is disabled or deleted.
Products cache answers briefly and drop them on `credential.revoked`.

### 3.9 Logout

#### RP-initiated logout

`GET` or `POST` (form) `/oidc/logout` on the realm's host (`oidc/logout.rs`). Parameters
`id_token_hint`, `client_id`, `post_logout_redirect_uri`, `state`, `ui_locales`.

- `id_token_hint` must be an id_token pacod-id signed for this realm (`typ` `JWT`, `iss` the
  realm's issuer); an expired one is accepted, one whose key has left the JWKS is not. A hint
  that does not verify is a 400 page.
- The client is the hint's `aud`, or `client_id`; both given and different is a 400 page.
- `post_logout_redirect_uri` must be one of that client's `post_logout_redirect_uris`, exactly;
  otherwise a 400 page. `state` is appended to it.

What happens:

| Browser session | Hint | Result |
| --- | --- | --- |
| live | names this session (`sid`) or its account (`sub`) | the session ends at once |
| live | none, or another session and account | `303` to the confirmation page `{issuer}/logout` with `client_id`, `post_logout_redirect_uri`, `state` and `ui_locales` |
| none | names a live session of its `sub` | that session ends |
| none | none | nothing to end |

Unless the confirmation page is shown, the answer clears the session cookie and is `303` to the
`post_logout_redirect_uri` (with `state`), or a "You are signed out" page without one. Ending a
session revokes every grant issued in it, emits `session.revoked` and queues back-channel
logouts. The confirmation page posts to `/ui-api/v1/logout` (section 6).

#### Server-to-server session revocation

`POST /internal/v1/sessions/{sid}/revoke`, internal listener, authenticated as in section 4.1. A
service client may end any session; a confidential client (HTTP Basic) only one in which it
held a grant (otherwise 403). The session and its grants end, with `session.revoked` and
back-channel logouts. The answer is always 204, also for an unknown or ended `sid`. A BFF that
keeps no id_token calls this and then sends the browser through
`/oidc/logout?client_id=…&post_logout_redirect_uri=…`, which then only clears the cookie and
redirects.

#### Back-channel logout

When sessions end, pacod-id queues a logout token for each client that has a
`backchannel_logout_uri` and held a grant (a refresh token) in one of them; a client that
received no refresh token in the session is not notified. The token (OpenID Back-Channel Logout
1.0):

- header `typ` `logout+jwt`, signed with the client's `id_token_alg`;
- claims `iss` (the client's realm), `aud` (the client id), `sub`, `sid`,
  `events: {"http://schemas.openid.net/event/backchannel-logout": {}}`, `iat`, `exp` (two
  minutes later) and `jti`, signed anew for each attempt.

Delivery (`deliver` in `oidc/logout.rs`, the outbox in `store/oidc.rs`): every instance looks
for due deliveries every 3 seconds, 20 at a time, each claimed for 30 seconds. It posts
`logout_token=<jwt>` form-encoded to the URI, with `Origin: <issuer's origin>` (so frameworks
that refuse cross-site form posts can allow it by name), a 5-second timeout and no redirects
followed. A 2xx answer is success. A token that cannot be signed fails that delivery alone
(recorded as `signing: …`), not the rest of the batch. Every failed or refused delivery is
logged as a warning. After a failure the next attempt waits 5 seconds × 2^attempts; the first
failure more than 10 minutes after the logout gives up.

---

## 4. Internal API for products' services

On the internal listener only.

### 4.1 Authenticating callers

Every `/internal/v1` call authenticates the calling client (`http/caller.rs`), with either:

- `Authorization: Bearer <token>` from the client-credentials grant: verified signature, `typ`
  `at+jwt`, `aud` `pacod-id`, `pid_kind` `service`, unexpired, `sub` a configured service
  client, and `iss` that client's realm's issuer; or
- `Authorization: Basic` with the client's id and secret, for any confidential or service
  client.

A missing or bad credential is 401 `unauthorized`. The catalogue, organisation, scope, feed and
deletion endpoints need a **service** client with a `product`, and it acts for that product
only (`acting_for`): another client kind is 403 `not_a_service`, another product 403
`wrong_product`. Introspection needs a service client. Session revocation also accepts
confidential clients (section 3.9).

Writes answer with the feed's `seq` after them. A product that has to read its own write waits
until its copy of the feed has reached that `seq`.

| Method | Path | Purpose |
| --- | --- | --- |
| `PUT` | `/internal/v1/catalog/{product}` | push the product's manifest |
| `GET` | `/internal/v1/catalog/{product}` | the stored manifest |
| `PUT` | `/internal/v1/organisations/{id}` | create or rename an organisation; register the product with it |
| `DELETE` | `/internal/v1/organisations/{id}?product=` | unregister the product from it |
| `PUT` | `/internal/v1/scopes/{kind}/{id}` | register or rename a product scope |
| `DELETE` | `/internal/v1/scopes/{kind}/{id}` | delete a product scope and everything below it |
| `GET` | `/internal/v1/authz/snapshot?product=` | the product's authorization data as of one `seq` |
| `GET` | `/internal/v1/authz/changes?product=&after=&wait=` | the changes after `after`, by long poll |
| `POST` | `/internal/v1/deletions/{sub}/done?product=` | the product has deleted its data about an account |
| `GET` | `/internal/v1/imports/{product}` | the product's last import and the identity writes since (4.7) |
| `POST` | `/internal/v1/introspect` | section 3.8 |
| `POST` | `/internal/v1/sessions/{sid}/revoke` | section 3.9 |

### 4.2 Catalogue

`PUT /internal/v1/catalog/{product}` (`authz/catalog.rs`), at every start of the product. The
body:

```json
{ "manifest": { "product": "gw", "scope_types": {}, "roles": {}, "grant": { "default": "members.manage" } },
  "migrate": { "gw.old_role": "gw.viewer", "gw.gone_role": null } }
```

`manifest` is the product's `authz/<product>.yaml` as JSON, in the format that
`crates/pacod-id-authz` (`manifest.rs`) checks; pacod-id's own roles,
`crates/pacod-id/src/authz/id.yaml`, are an example. The same manifest again changes nothing
(`"changed": false`). A manifest that drops a role, or no longer allows it at a scope type, while bindings use it is refused with 409 `role_in_use` unless
`migrate` names the role: its bindings then move to the given role, or are removed for `null`.
Dropping a scope type with registered scopes is 409 `scope_type_in_use`; dropping custom roles
while some exist is 409 `custom_roles_in_use`. The product `id` is reserved (403
`wrong_product`), and the manifest's `product` must be the path's (else 403 `wrong_product`).

Answer: `{"product", "version", "changed", "seq"}`. Every instance picks up a new catalogue
within 10 seconds. `GET` returns `{"product", "version", "manifest", "pushed_at"}`, or 404
before the first push.

### 4.3 Deletion confirmations

`POST /internal/v1/deletions/{sub}/done?product=<product>`: the product has deleted its data
about the deleted account `sub` (`admin/deletion.rs`). `product` must be the caller's (403
`wrong_product`). Once every product that had pushed a catalogue at the time of the deletion
has confirmed, the tombstone goes. Always 204.

The deletions still waiting for the product are in the snapshot's `deleted` (section 4.5). The
`AuthzCache` of `pacod-id-client` (Rust) and of `pacod_id.cache` (Python) announces each of
them on every snapshot load (at start, and after the feed answered 410) as a
`principal.deleted` event with `sub` and `from_snapshot: true`, but without `principal_kind`
and `external_ids`. A product that missed the feed's event can thus erase by `sub` and confirm;
what it erases by external id it has to find from its own records.

### 4.4 Organisations and scopes

Products register organisations and their own scopes with their own ids, after their own
transaction commits; every call is idempotent (`authz/scopes.rs`).
Ids may not be empty, longer than 100 characters, or contain `:`, `/` or whitespace
(422 `bad_id`). The four calls' own validation errors have `type` `scope_error`; a caller that
is not the product's service gets 403 `forbidden` (`not_a_service`, `wrong_product`).

`PUT /internal/v1/organisations/{id}`, body `{"slug"?, "display_name"?, "system"?}`: creates the
organisation or updates it, and registers the calling product with it. `slug` defaults to the
id and must be unique (409 `slug_taken`). Emits `scope.upserted` when something changed.

`DELETE /internal/v1/organisations/{id}?product=<product>`: unregisters the calling product
(`product` is optional and, when given, must be the caller's). Once no product has the
organisation, it is deleted with every scope, binding and invitation below it. While other
products keep it, the calling product's own part goes: its scopes below the organisation (each
`scope.deleted`, with their bindings and invitations), and its roles and invitations at the
organisation itself (`binding.removed`). Then `scope.upserted` is emitted with the new
`products` (nothing at all when the calling product was not registered with it). A system organisation is 409 `system_scope`; an unknown
one is a no-op.

`PUT /internal/v1/scopes/{kind}/{id}`, body `{"parent", "display_name"?, "system"?}`: `kind` is
one of the product's scope types (`gw.workspace`; another product's is 403 `wrong_product`, and
409 `catalog_missing` before the product's first push). `parent` is written `platform`,
`organisation:<id>` or `<product>.<type>:<id>`, of the type the manifest names (422
`bad_parent`), and must be registered (409 `scope_unknown`). A scope's parent never changes (409
`parent_changed`).

`DELETE /internal/v1/scopes/{kind}/{id}`: deletes the scope, the scopes below it, their bindings
and invitations, emitting `binding.removed` and `scope.deleted`. A system scope is 409
`system_scope`; an unknown one is a no-op.

All four answer `{"seq": n}`: the version of the last event written, or the current head when
nothing changed.

### 4.5 Snapshot

`GET /internal/v1/authz/snapshot?product=<product>` returns, in one repeatable-read
transaction, everything the product needs as of `seq` (`authz/feed.rs`):

| Field | Content |
| --- | --- |
| `seq` | the feed version the data is at |
| `product` | the product |
| `catalog` | `{"version", "manifest"}`, or null before the first push |
| `scopes` | every organisation: `{type: "organisation", id, slug, display_name, status, system, products, path}`; and the product's scopes: `{type, id, parent, display_name, system, path}` |
| `bindings` | the bindings of the product's roles and of `id.*` roles: `{id, principal, principal_kind, role, path, source}` |
| `disabled` | ids of accounts and service accounts that are not active |
| `deleted` | ids of deleted accounts this product has not yet confirmed (section 4.3) |
| `revoked_sids` | `{sid, sub}` of sessions revoked in the last 15 minutes |

A `path` is the scope's chain from the platform: `["platform", "organisation:org_1",
"gw.workspace:ws_1"]`. A binding whose scope is no longer registered is left out.

### 4.6 Changes

`GET /internal/v1/authz/changes?product=<product>&after=<seq>&wait=<seconds>`. `product` and
`after` are required; `wait` defaults to 0 and is capped at 30. The call answers at once when
there are changes after `after`; otherwise it waits up to `wait` seconds for one. Each instance
wakes its waiting requests through PostgreSQL `LISTEN pid_authz`, and polls every second while
that connection is down. At most 1000 changes are read per answer.

```json
{
  "seq": 1843,
  "events": [
    { "seq": 1843, "at": "2026-10-12T09:14:03.512Z", "kind": "binding.removed",
      "id": "rb_0192…", "principal": "usr_0191…", "principal_kind": "user", "role": "gw.viewer",
      "path": ["platform", "organisation:org_1", "gw.workspace:ws_1"], "source": "manual" }
  ]
}
```

The top-level `seq` is how far the follower has read. It can be ahead of the last event's `seq`,
and it advances while `events` is empty, because events of other products are read and left
out. A follower always sends the top-level `seq` as its next `after`.

A product receives the events of its own product, of `id`, and those of no product (sessions,
principals, accounts, organisations). Every event has `seq`, `at` and `kind`, plus:

| Kind | Fields | Product |
| --- | --- | --- |
| `binding.added` | `id`, `principal`, `principal_kind`, `role`, `path`, `source` | the role's |
| `binding.removed` | `id`, `principal`, `principal_kind`, `role`, `path`, `source` | the role's |
| `principal.disabled`, `principal.enabled` | `sub`, `principal_kind` | none |
| `principal.deleted` | `sub`, `principal_kind`; for an account also `external_ids` (`{source: id}`) | none |
| `session.revoked` | `sid`, `sub`, `reason` | none |
| `credential.revoked` | `token_id`, `sub` (the service account) | none |
| `scope.upserted` (organisation) | `type`, `id`, `slug`, `display_name`, `status`, `system`, `products`, `path`, as in the snapshot | none |
| `scope.upserted` (product scope) | `type`, `id`, `parent`, `display_name`, `system`, `path` | the scope's |
| `scope.deleted` | `type`, `id`, `path` | the scope's, none for an organisation |
| `catalog.updated` | `product`, `version` | the product's |
| `account.updated` | `sub` | none |

`session.revoked` reasons include `logout`, `ended_by_owner`, `password_changed`,
`password_reset`, `password_set_by_admin`, `refresh_token_reused`, `account_disabled` and
`account_deleted`. `account.updated` is emitted when an account's name, language or address
changes (through the admin API, the account page, or a confirmed address change), never when
nothing changed; a service account's rename emits no event.

**When the cursor is too old.** Changes are kept 7 days. When `after` is older than the oldest
change kept, or newer than the feed's head (for example after the database was restored), the
answer is 410:

```json
{ "error": { "type": "gone", "message": "the changes after this seq are no longer kept; load the snapshot",
             "details": { "code": "cursor_expired" } } }
```

The follower then loads the snapshot again, replaces its copy with it, and continues with
`after` set to the snapshot's `seq`. `pacod-id-client` does this (`cache.rs`).

---

### 4.7 Imports

`pacod-id import gw <file> [--dry-run] [--realm gateway]` takes over the gateway's own people
from `pacod-gw-core identity export` (docs/runbook.md section 19). It does not migrate the
database: run it against a database the service has brought up to its version. `GET
/internal/v1/imports/{product}` (the caller's product only) answers how the last import stands:

```json
{ "imported_at": "2026-10-13T08:05:12Z", "feed_seq": 311, "source_version": 42,
  "exported_at": "2026-10-13T08:00:00Z", "writes": 0, "first": [] }
```

`writes` counts what was written at pacod-id since, which a return to the product's own sign-in
would lose: audit entries in the import's realm, or about (or by) the accounts and service
accounts the import brought wherever they happened, other than signing in and out, sessions and
mail sent; and the feed's changes to the product's roles and to those people (not to scopes,
nor to other products' people). `first` names up to ten of them. A product never imported: 404
`no_import`.

An import records what it brought. A later import (after a rollback) makes pacod-id match the
new export: what the product removed since is taken away here too (roles and invitations
removed, accounts and service accounts disabled, their tokens revoked), changed names and
addresses are announced (`account.updated`), and the sessions of accounts whose password changed
or which are disabled now end. An import never changes an account, a service account or an
invitation by an id it did not bring itself (a conflict), nor one in another realm than the
last import's.

## 5. Admin API

On the internal listener at `/admin/v1`, and on the public listener at `/ui-api/v1/admin` for
the hosted admin pages (section 5.6). Products' facades call it (section 5.5). There is no
API for clients: clients are static configuration.

### 5.1 The actor

Every call acts as someone, and every decision is made on that someone's own bindings
(`admin/actor.rs`). On `/admin/v1` the actor comes from `Authorization: Bearer <token>`, where
the token is one a product's service obtained by token exchange for `audience=pacod-id`
(section 3.4):

- verified signature, `typ` `at+jwt`, `aud` `pacod-id`, unexpired, `iss` a configured realm's
  issuer: that realm is the actor's realm;
- `act.sub` names a configured client: the exchanging service, whose `product` is the actor's
  product (it limits some answers to that product's roles, below);
- `pid_kind` `user`: the person, while the token's session is live and the account active;
- `pid_kind` `service`: the service account `sub`, while it is active.

A client-credentials token has no `act` and is refused: the product's service itself gains
nothing. Any failure is 401 with `details.code` `invalid_token`. A forbidden action is 403
`not_allowed`, except where the API answers 404 so that ids reveal nothing.

### 5.2 Permissions

pacod-id's own roles are in `crates/pacod-id/src/authz/id.yaml`: `id.admin` (every `id.*`
permission, protected: nobody removes the last one) and `id.auditor` (`id.audit.read`,
`id.accounts.read`), both at the platform. Its permissions are `id.accounts.read`,
`id.accounts.manage`, `id.service_accounts.manage`, `id.bindings.manage`, `id.roles.grant` (the
grant permission of `id.*` roles) and `id.audit.read`.

The checks the handlers use:

| Check | True when the actor |
| --- | --- |
| reads accounts | has `id.accounts.read` at the platform, or the realm policy |
| manages an account | has `id.accounts.manage` at the platform; or has the realm policy, the account signed up in the actor's realm, and the actor could grant every role the account holds (else 403 `holds_more_rights`; the actor's own account is exempt) |
| administers a path | has `id.bindings.manage` at the platform, or the acting product's grant default permission (the gateway's `gw.members.manage`) at the path |
| manages service accounts at a path | has `id.service_accounts.manage` there, or administers it |
| may grant a role at a path | passes the role's grant rule in its manifest at the path, or has `id.bindings.manage` at the platform |
| sees a scope's members | has `id.accounts.read` at the platform (the realm policy does not count here), administers the path, or holds a manifest role (not a custom role) of a product concerned at the scope or above it |

**The realm policy** (`accounts_managed_by`): a realm may name roles, for example
`gw.platform_admin`. An actor holding one of them at the
platform manages the accounts that signed up in the actor's realm as if it held
`id.accounts.read` and `id.accounts.manage`, and sees only those. Apart from its own account,
it may change, mail or delete only an account all of whose bindings it could have given, by
each role's grant rule at the binding's scope (`can_grant`; the actor's `id.bindings.manage`
does not count here):
setting the password of an identity admin, or of an administrator of more scopes, would make the
actor that person. Otherwise the answer is 403 `holds_more_rights`. Holders of
`id.accounts.manage` at the platform are not limited this way.

### 5.3 Routes

Paths are relative to `/admin/v1` (and `/ui-api/v1/admin`). "Product" means the call came
through a product, so the actor has a product.

| Method | Path | Needs | Purpose |
| --- | --- | --- | --- |
| `GET` | `/accounts?q=&after=&limit=` | reads accounts | list accounts by address, filtered by `q` (address or name), 50 by default, at most 500; `next` is the `after` of the next page |
| `POST` | `/accounts` | manages accounts (creates in the actor's realm) | create an account |
| `GET` | `/accounts/{id}` | reads accounts, the account visible | one account |
| `PATCH` | `/accounts/{id}` | manages the account | change name, language, address, password, status |
| `DELETE` | `/accounts/{id}` | manages the account | delete the account and its data |
| `POST` | `/accounts/{id}/verification` | manages the account | mail a confirmation link (202) |
| `POST` | `/accounts/{id}/password-reset` | manages the account | mail a reset link (202) |
| `POST` | `/accounts/{id}/email-confirmation` | manages the account | mark the address confirmed (204) |
| `GET` | `/service-accounts?organisation=` | manages service accounts at the platform or organisation | list service accounts |
| `POST` | `/service-accounts` | the same, at the new account's scope | create a service account |
| `GET` | `/service-accounts/{id}` | manages its scope (else 404) | one service account |
| `PATCH` | `/service-accounts/{id}` | manages its scope | rename, disable, enable |
| `DELETE` | `/service-accounts/{id}` | manages its scope | delete it with its bindings and tokens |
| `GET` | `/service-accounts/{id}/tokens` | manages its scope | its tokens, without secrets |
| `POST` | `/service-accounts/{id}/tokens` | manages its scope | create a `pidt_` token; the secret is shown once |
| `DELETE` | `/service-tokens/{id}` | manages its account's scope (else 404) | revoke a token |
| `GET` | `/bindings?principal=&scope=` | | bindings (the actor's own without parameters); each shown if it is the actor's or the actor sees its scope's members |
| `POST` | `/bindings` | may grant the role at the scope | add a binding |
| `DELETE` | `/bindings/{id}` | may grant its role at its scope | remove a binding |
| `GET` | `/scopes/{kind}/{id}/members` | sees the scope's members | members of a scope |
| `PUT` | `/scopes/{kind}/{id}/members/{principal}` | may grant the roles replaced and the role given | set a principal's one role of a product at the scope |
| `DELETE` | `/scopes/{kind}/{id}/members/{principal}?below=` | may grant each role removed | remove a principal's roles at the scope (and below, by default) |
| `GET` | `/invitations?scope=&below=` | administers the scope | pending invitations at the scope (and below, by default) |
| `POST` | `/invitations` | may grant the role at the scope | invite an address to a role |
| `DELETE` | `/invitations/{id}` | may grant its role (else 404) | revoke an invitation (204) |
| `POST` | `/invitations/{id}/resend` | may grant its role (else 404) | a new link; the old one stops working |
| `GET` | `/custom-roles?product=` | `id.bindings.manage`, or the named product's grant default permission, at the platform | the product's custom roles |
| `POST` | `/custom-roles` | the same | create a custom role |
| `PATCH` | `/custom-roles/{product}/{name}` | the same | rename (bindings follow) or describe |
| `DELETE` | `/custom-roles/{product}/{name}` | the same | delete it and its bindings |
| `GET` | `/audit?actor=&subject=&action=&before=&limit=` | `id.audit.read` at the platform | the identity audit log, newest first |

For the members routes, the platform is written `kind` `platform` with any `id`. Changes that
write the feed answer with `seq` (the version of their last event, or null when nothing
changed). Every change is recorded in the audit log with the actor and, through a product, its
client.

### 5.4 Requests and answers

**Accounts** (`admin/accounts.rs`). An account is shown as `{id, email, display_name, locale,
status, email_verified, email_verified_source, has_password, signup_realm, last_realm,
created_at, last_login_at}`.

- `POST /accounts` takes `{"email", "display_name"?, "locale"?, "password"?}` and answers 201.
  The account signs up in the actor's realm. Without a password, the person sets one through a
  reset link or an invitation. The address counts as confirmed when the installation sends no
  mail or the realm does not require confirmation; otherwise a confirmation link is mailed. 409
  `email_taken`; 422 `bad_email` or a password code.
- `PATCH /accounts/{id}` takes any of `{"display_name", "locale", "email", "password",
  "status"}` and answers the account with `seq` (of the last event written, or null). Every
  value is checked before anything is saved, so a refused field leaves the account as it was:
  `status` (422 `bad_status`), `locale` (422 `bad_locale`), `email` (422 `bad_email`, 409
  `email_taken`) and the password policy. Then the fields are applied in the order status, name
  and language, address, password. `status` `disabled` checks the last-holder rule (409
  `last_holder`), ends every session and emits `principal.disabled`; `active` emits
  `principal.enabled`. A changed name or language emits `account.updated`. A new `email` is
  confirmed only when actors change their own or no mail is sent, the old address is told, and
  `account.updated` is emitted. A new `password` must meet the role holders' minimum when the
  account holds roles, ends every session and mails the owner. Disabling and a new password
  also delete the account's pending address-change links.
- `DELETE /accounts/{id}` refuses while the account is the last holder of a protected role (409
  `last_holder`), ends its sessions (with back-channel logouts), deletes the account and what
  belongs to it, emits `principal.deleted` with its external ids and answers `{"seq"}`.
- The mail of `verification`, `password-reset` and address changes goes out in the account's
  last realm, else its sign-up realm, else the actor's. Without mail, `verification` and
  `password-reset` answer 503 `email_unavailable`; `verification` of a confirmed address is 409
  `already_verified`, `password-reset` of a disabled account 409 `account_disabled`.

**Service accounts** (`admin/service.rs`). Shown as `{id, name, organisation_id, status,
created_by, created_at}`. `POST` takes `{"name", "organisation_id"?}` (a name of 1 to 100
characters; without an organisation it belongs to the platform; an unregistered organisation is
409 `scope_unknown`). `PATCH` takes `{"name"?, "status"?}` and checks both values (422
`bad_name`, `bad_status`) before saving either; a rename is saved first and emits no event,
then disabling checks the last-holder rule (409 `last_holder`, the rename stays) and emits
`principal.disabled`. `DELETE` removes its bindings, revokes its tokens
(`credential.revoked` each) and emits `principal.deleted`. Tokens are listed as `{id,
service_account_id, hint, legacy_product, expires_at, last_used_at, status, created_at}`.
`POST …/tokens` takes `{"expires_at"?}` (RFC 3339) and answers 201 `{"id",
"service_account_id", "token", "hint", "expires_at"}`; `token` is shown only here. `DELETE
/service-tokens/{id}` emits `credential.revoked`.

**Bindings and members** (`admin/members.rs`). A binding is shown as `{id, principal,
principal_kind, role, scope, path, source, created_by, created_at}`. `POST /bindings` takes
`{"principal", "role", "scope"}` and answers 201 with the binding and `seq`. The principal
holds it already: 409 `binding_exists`; unknown principal: 404 `principal_unknown`;
unregistered scope: 409 `scope_unknown`; a role that cannot be held there: 422
(`unknown_role`, `implicit_role`, `scope_type_not_allowed`, `bad_path`). Any change that would
leave a scope without a holder of a protected role is 409 `last_holder`, unless the role's
manifest gives the actor a bypass.

`GET /scopes/{kind}/{id}/members` answers `{"scope", "members": [{principal, principal_kind,
email, name, status, roles: [{binding, role, scope, inherited}]}]}`: everyone with a binding at
the scope or below it, plus, marked `inherited`, the bindings at the scopes above it except the
platform. Through a product, only that product's roles are listed. `PUT
…/members/{principal}` takes `{"role": "<role>" | null}` and replaces the principal's roles of
that role's product (or the acting product's, for `null`) at exactly this scope; roles below
stay. Through the cookie mount there is no acting product, so `null` removes all the
principal's roles at the scope. The actor's right to grant the role given and every role
replaced is checked first (403 `not_allowed`); only then does a request that changes nothing
answer `{"seq": null}`, so that answer tells nobody who holds what. `DELETE
…/members/{principal}` removes the principal's roles at the scope and, unless `below=false`,
below it (through a product, only that product's); 404 `not_a_member` when there are none.

**Invitations** (`admin/invitations.rs`). `POST /invitations` takes
`{"email", "role", "scope", "deliver"?, "labels"?}`. `deliver` is `email` (the default when the
installation sends mail) or `link`; `labels` (`{"role": "Viewer", "scope": "Acme"}`) name the
role and scope in the mail and on the page. One invitation per address and scope: a new one
replaces a pending (unexpired) one only when both roles are of the same product (by the role's
prefix) and the actor could grant the pending role too; otherwise the answer is 409
`invitation_exists` with the pending invitation's `role`. An expired one is replaced in any
case. The answer (201) is `{id, email, role, role_label, scope,
scope_label, realm, invited_by, inviter_name, emailed, created_at, expires_at, expired}`
(`inviter_name`: the inviting account's display name or address, or the service account's
name; empty once the inviter is gone), plus `token` and
`link` when the link was not mailed (shown once). It lives `invitation_days` (default 7).
Through a product, `GET /invitations` lists only that product's roles. `resend` takes
`{"deliver"?}` and answers like `POST`.

**Custom roles** (`admin/roles.rs`). Only for a product whose
manifest allows them (else 422 `custom_roles_off`; a product without a catalogue is 404). The
check uses the product named in the request, whichever product the call came through. Shown as
`{product, name, role: "<product>.custom.<name>", description}`. `POST` takes `{"product",
"name", "description"?}`; names are lowercase letters, digits and `_`, starting with a letter,
at most 40 characters.
`PATCH` takes `{"name"?, "description"?}`; renaming moves every binding to the new role
(`binding.removed` and `binding.added`).

**Audit** (`admin/audit.rs`). `GET /audit` filters by `actor`, `subject` and `action` exactly;
`limit` defaults to 100, at most 500; `before` takes the previous page's `next`. Entries are
`{id, at, actor, actor_kind, action, subject, client_id, realm, ip, detail}`. There is
no per-organisation slice of the log for products.

### 5.5 Through a product

A product's facade exchanges its caller's token, calls `/admin/v1`, and passes errors on in the
same shape. The actor's `product` narrows the members and invitation lists and the members
changes to that product's roles, and makes the product's grant default count as administering a
scope. It does not let the service act for anyone who did not call it.

### 5.6 The hosted admin pages

The same routes are mounted at `/ui-api/v1/admin` on the public listener. There the actor is the
person of the realm's session cookie (`pid_sid`), with that realm; there is no product, so only
`id.*` permissions and the realm policy count. Writes (every method but `GET` and `HEAD`) need
the realm's own `Origin` (403 `cross_origin`). Without a session: 401 `sign_in_required`.

---

## 6. The hosted pages' API

`/ui-api/v1` on the public listener serves pacod-id's own pages (`/login`, `/signup`,
`/verify-email`, `/reset-password`, `/forgot-password`, `/invite`, `/logout`, `/account/*`,
`/admin/*`). It is not a contract for products and may change with
the pages. It is listed here so that operators and reviewers know what is exposed.

- **Same origin.** Every write (`POST`, `PATCH`, `DELETE`) must carry an `Origin` equal to the
  realm's origin (`scheme://host[:port]` of the issuer), else 403 `cross_origin`. Reads need no
  `Origin`. Bodies are JSON.
- **Cookies.** The flow endpoints need the `pid_flow` cookie of the browser that started the
  flow (else 410 `flow_expired`); the account endpoints need the session cookie `pid_sid` (else
  401 `sign_in_required`). The pages set `pid_lang` and `pid_theme`, which the server reads to
  render the page.
- **Throttles.** Wrong passwords: 30 per 15 minutes per address
  (IPv4, or IPv6 /64), and 10 per 15 minutes per account from browsers it has not signed in
  on, counted separately per known browser; the attempt after the limit is refused and starts
  a lock of 15 minutes (429 `too_many_attempts`). Each attempt takes its place in the counters
  before the password is checked, and a correct password gives the address's place back and
  clears the account's counter, so parallel attempts cannot get past the limit. The account
  pages' password checks (`reauth`, `password`) have their own counter, 10 per 15 minutes per
  account, kept the same way. Mail: 3 per 15 minutes per purpose and address, 20 an hour per
  client address, the extra ones dropped silently. Sign-ups: 10 an hour per client address. The
  client address is the peer, or the `X-Forwarded-For` hop after the proxies
  `PID_TRUSTED_PROXIES` names.
- **Answers that move the browser** are `{"next": "redirect", "to": <url>}`, plus the cookies to
  set. Other `next` values: `verify_email`, `change_password` (with `min_length`), `sign_in`,
  `signed_out`, `continue`, `check_email`.

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/ui-api/v1/realm` | the realm's name, brand, languages, sign-up, Google, legal links, password minimums |
| `GET` | `/ui-api/v1/flows/{id}` | a sign-in flow: stage, realm, client, hints, sign-up allowed |
| `POST` | `/ui-api/v1/flows/{id}/password` | `{email, password, remember_me}`: the password step |
| `POST` | `/ui-api/v1/flows/{id}/password-change` | `{password}`: a new password when one is due |
| `POST` | `/ui-api/v1/flows/{id}/resume` | continue after the address was confirmed; `{"next": "verify_email", "email"}` while it is not |
| `POST` | `/ui-api/v1/flows/{id}/signup` | `{email, password, display_name, locale, accept_terms}` |
| `POST` | `/ui-api/v1/flows/{id}/google` | start Google sign-in; answers the redirect to Google |
| `GET` | `/federation/google/callback` | Google sends the browser back here (`code`, `state`) |
| `POST` | `/ui-api/v1/verification` | `{email}`: mail a confirmation link again (always 202) |
| `POST` | `/ui-api/v1/verification/confirm` | `{token}`: confirm an address, or apply an address change |
| `POST` | `/ui-api/v1/password-reset` | `{email}`: mail a reset link (always 202) |
| `POST` | `/ui-api/v1/password-reset/confirm` | `{token, password}`: set the password, end every session |
| `POST` | `/ui-api/v1/logout` | `{client_id, post_logout_redirect_uri, state}`: the sign-out confirmation; ends the cookie's session only when it is of this realm |
| `POST` | `/ui-api/v1/invitations/lookup` | `{token}`: what an invitation link offers |
| `POST` | `/ui-api/v1/invitations/accept` | `{token, password?, display_name?}`: accept, signed in or with a new account |
| `GET` | `/ui-api/v1/account/invitations` | the signed-in person's pending invitations of this realm; none while the address is not confirmed |
| `POST` | `/ui-api/v1/account/invitations/{id}/accept` | accept one (confirmed address, this realm; else 404) |
| `POST` | `/ui-api/v1/account/invitations/{id}/decline` | decline one (204; the same conditions) |
| `POST` | `/ui-api/v1/account/sign-in` | `{return_to?, create?, login_hint?}`: a sign-in for pacod-id's own pages |
| `GET`, `PATCH`, `DELETE` | `/ui-api/v1/account` | the account and which admin pages to show; name and language (with `seq`, null when nothing changed); delete the account (recent sign-in) |
| `POST` | `/ui-api/v1/account/reauth` | `{password}`: counts as a recent sign-in for ten minutes |
| `POST` | `/ui-api/v1/account/password` | `{current?, password}`: a new password; other sessions and pending address-change links end |
| `POST` | `/ui-api/v1/account/email` | `{email}`: a link to the new address (recent sign-in); 202 also for a taken address, which gets no link |
| `GET` | `/ui-api/v1/account/sessions` | the account's live SSO sessions (not the grants of native apps) |
| `DELETE` | `/ui-api/v1/account/sessions/{sid}` | end one session (204) |
| `POST` | `/ui-api/v1/account/sessions/others` | end every other session and grant |
| `GET` | `/ui-api/v1/account/organisations` | the account's roles with the names of their scopes |
| `GET` | `/ui-api/v1/account/connections` | password and Google |
| `DELETE` | `/ui-api/v1/account/connections/google` | unlink Google, unless it is the only way to sign in |
| any | `/ui-api/v1/admin/*` | the admin API, section 5.6 |

Sign-up never says whether an address is taken when the installation sends mail: a taken one
gets the same answer, `{"next": "verify_email", "email"}`, and its owner a mail. The flow is
left in the same state for both, at stage `verify_email` with `login_hint` set to the address;
for a taken address it is bound to no account, so `resume` keeps answering `verify_email` and
the flow never completes. (Without mail a new address signs in at once, so the answers differ;
docs/runbook.md section 13.)

Changing one's address (`POST /ui-api/v1/account/email`) does not tell either: the request is
counted first (3 links per 15 minutes per account), and the answer is 202 whether or not
another account has the address; a taken address gets no link. Pending address-change links
are deleted by a password reset, by a password change in a sign-in flow or on the account page,
and by an administrator disabling the account, setting its password or deleting it. Confirming
a link (`/ui-api/v1/verification/confirm`) changes the address only for the request that takes
the link; another one at the same moment gets 410 `link_invalid`.

Google sign-in links an existing account by address only when Google confirmed the address and
the account's is confirmed, or after the account's password; where sign-up is closed (by the
realm or the client) it creates an account only for an address with a pending invitation.

---

## 7. Health and metrics

On the metrics listener (`http/mod.rs`):

| Path | Answer |
| --- | --- |
| `GET /healthz` | 200 `ok` while the process serves requests |
| `GET /readyz` | 200 `ready` when start-up is done and shutdown has not begun, PostgreSQL answers, and an RS256 signing key is loaded; otherwise 503 `not ready` |
| `GET /metrics` | the Prometheus text format |

Start-up connects to PostgreSQL, migrates the schema, loads or creates the signing keys and the
catalogue, and creates the bootstrap admin; `/readyz` turns 503 again as soon as shutdown
begins, while requests in flight finish.

The metrics pacod-id records:

| Metric | Kind | Labels | Meaning |
| --- | --- | --- | --- |
| `pid_http_requests_total` | counter | `listener` (`public`, `internal`), `status` (`2xx` … `5xx`) | requests answered |
| `pid_http_request_seconds` | summary (no buckets are configured, so the exporter renders quantiles) | `listener` | request durations |

The metrics listener's own requests are not counted. There are no other application metrics;
operators watch the feed's freshness from the products' side.
