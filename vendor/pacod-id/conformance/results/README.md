# OpenID conformance results

The OpenID Foundation's conformance suite, release v5.3.2 (its published images), against the
pacod-id image built from the working tree of 2026-10-12 (stage 10, after the review's fixes), run
locally with `conformance/run.sh config basic logout backchannel`. The suite's exports (zip files
with every test's log, signed by the suite) are written next to this file by each run and are
not committed.

These are local runs, not a certification: certification is submitted from the suite's hosted
instance against a public deployment.

## How the run differs from a plain one

- **TLS.** The suite wants an https issuer. Caddy with its internal CA terminates TLS in front of
  pacod-id at `https://pid.oidf.test:27470`; the suite accepts any certificate.
- **PKCE.** pacod-id requires PKCE (S256) of every client. The suite's OpenID
  Connect plans never send it, so `conformance/pkce-shim.mjs` adds a fixed challenge to an
  authorization request without one and the matching verifier to the code exchange. Requests
  that carry their own PKCE (the suite's PKCE test) pass untouched. pacod-id's own tests cover
  refusing a request without PKCE.
- **The browser.** The suite's own browser (HtmlUnit) does not run the hosted pages, so the
  plans run without its browser block and `conformance/drive.mjs` plays the person in Chromium:
  it opens the URLs a test asks for, signs in, confirms a sign-out, and uploads a screenshot to
  each test that asks for one.
- **The test account** is the bootstrap admin, with a display name set so that the profile
  scope has a name to return. It has no address or phone claims; pacod-id offers neither.
- **Clients.** Two static confidential clients, `oidf-1` and `oidf-2`, with client_secret_basic
  (and client_secret_post for the test of it).

## Results

| Plan                                                                       | Modules | Passed | Review | Warning | Skipped | Failed or blocked |
| -------------------------------------------------------------------------- | ------: | -----: | -----: | ------: | ------: | ----------------: |
| Config OP (`oidcc-config-certification-test-plan`)                         |       1 |      1 |      0 |       0 |       0 |                 0 |
| Basic OP (`oidcc-basic-certification-test-plan`, discovery, static client) |      35 |     20 |      4 |       7 |       4 |                 0 |
| RP-Initiated Logout OP (response_type code, static client)                 |      11 |      3 |      8 |       0 |       0 |                 0 |
| Back-Channel Logout OP                                                     |       2 |      1 |      0 |       0 |       0 | 1 blocked (below) |

Conditions checked: 33 (Config), 1,779 (Basic), 558 (RP-Initiated Logout); no condition failed.

**Review** means a test finished and left a screenshot for a person to judge. They were looked at:
the sign-in page shown again for `prompt=login` and `max_age=1`; pacod-id's error page, not a
redirect, for an unregistered redirect URI, for a request object with a redirect URI, and for an
unregistered post-logout URI; the confirmation or "Signed out" page for the logout requests
without an `id_token_hint` or without a post-logout URI. Each shows what the test expects.

**Skipped**: the address, phone and "all scopes" tests (pacod-id has no address or phone
claims) and the unsigned request object test (request objects are refused with
`request_not_supported`, which the test accepts).

### Warnings, each a deliberate choice

| Test                                          | Warning                                                                | Why                                                                                                                                   |
| --------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| oidcc-userinfo-post-body                      | the access token in a POST body is not accepted at userinfo            | RFC 6750 says the body method should not be used where the header can be; pacod-id takes the header only.                             |
| oidcc-scope-email, oidcc-alternate-happy-flow | `email` in the id_token for `scope=email`                              | The products' sign-in libraries read the address from the id_token; OIDC allows it there.                                             |
| oidcc-scope-profile                           | the profile claims returned are `name` and `locale`, not the full list | pacod-id keeps no given name, picture, birthdate and the like.                                                                        |
| oidcc-claims-essential                        | `name` not in userinfo when asked as essential through `claims`        | The `claims` parameter is not supported (discovery says so); `name` comes with the profile scope.                                     |
| oidcc-ensure-request-with-acr-values-succeeds | no `acr` claim                                                         | Authentication levels come with MFA (phase 4); discovery does not list `acr`.                                                         |
| oidcc-codereuse-30seconds                     | the access token still works at userinfo after the code was reused     | Access tokens are self-contained JWTs of 5 minutes; a reused code revokes the refresh token and the grant, not tokens already issued. |

### Back-Channel Logout: blocked by the suite's certificate

pacod-id sent the logout token to the suite's back-channel endpoint and retried it, but every
attempt failed:

```
error sending request for url (https://localhost.emobix.co.uk:27443/test/a/pacod-id/backchannel_logout):
client error (Connect): invalid peer certificate: Other(OtherError(CaUsedAsEndEntity))
```

The suite serves a self-signed certificate; pacod-id verifies certificates (rustls with the
Mozilla roots) and is not given an option to stop. The test therefore waits until it times out.
The back-channel logout itself is covered by pacod-id's tests (`crates/pacod-id/tests/sessions.rs`)
and by the end-to-end run (`e2e/fixtures.spec.ts`: three fixture applications, one of them in a
container, signed out by pacod-id). The plan can be run against a deployment whose suite endpoint
has a public certificate, or with the suite's certificate added to the image's trust store.

## Found by these runs and fixed

- A profile scope sent `"name": ""` for an account without a name; empty claims are now left out
  (stage 10, part B).
