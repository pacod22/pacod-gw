# Measurements

`make measure` (`scripts/measure.mjs`) runs against the development stack (`make compose-up`):
the release image, musl and mimalloc, PostgreSQL 18 in a container, pacod-id connected as its
own non-superuser role. The script fails when a target is missed; CI runs it in the `image` job.

## Targets

| What | Target |
| --- | --- |
| Token endpoint, password hashing excluded | p99 under 50 ms |
| Feed: a change reaches a follower already waiting on `/internal/v1/authz/changes` | p99 under 1 s |

"Password hashing excluded": the refresh and client-credentials grants. A sign-in's Argon2id
(19 MiB, 2 passes: OWASP's minimum) is a deliberate cost and is measured by nothing here.

## 2026-10-12, pacod-id 0.1.0 (S10, after the review's fixes)

Docker Desktop on an Apple-silicon Mac, every call from the host through Docker's port
forwarding, 500 rounds after 50 warm-up rounds.

| Measured | p50 | p95 | p99 | max |
| --- | ---: | ---: | ---: | ---: |
| client_credentials, one caller | 0.7 ms | 0.8 ms | 0.9 ms | 2.5 ms |
| client_credentials, 8 callers at once | 1.3 ms | 2.7 ms | 5.4 ms | 8.8 ms |
| refresh_token (rotation), one caller | 1.8 ms | 2.1 ms | 2.5 ms | 10.1 ms |
| refresh_token (rotation), 8 callers at once | 3.0 ms | 4.7 ms | 6.6 ms | 8.8 ms |
| feed: scope registered → waiting follower (200 rounds) | 1.5 ms | 3.1 ms | 10.2 ms | 12.2 ms |

The run before the fixes (the same day) gave p99 1.2, 8.2, 6.0, 5.4 and 3.4 ms in the same
order. A refresh now also keeps the hash of the token it supersedes, for reuse detection.

Each token is an RS256 JWT signed by ring. A refresh also rotates the refresh token: one read,
two inserts and one update in a transaction. The feed's latency is `LISTEN pid_authz` waking the
long-poll, which then reads the new rows.

The feed figure is the service's own share. A product's follower adds its own handling. The
products' end-to-end revocation target (2 seconds from sign-out to refusal) depends on each
product's follower and is measured there.
