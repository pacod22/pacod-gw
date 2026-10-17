# pacod-id

One place where people and machines sign in, and where it is recorded what they may do, for
several products at once: an OpenID Connect provider for first-party applications, with accounts,
organisations, role bindings, and a change feed that products follow to decide access locally.

The documentation and the tests use the gateway, pacod-gw (product `gw`: organisations that hold
workspaces, and a console), and a fictional product, `shop`, whose people sign in under two brands,
`shop` and `bazaar`.

Running it: [docs/runbook.md](docs/runbook.md). Integrating with it: [docs/api.md](docs/api.md). What protects what: [docs/threat-model.md](docs/threat-model.md).
Latency: [docs/measurements.md](docs/measurements.md). OpenID conformance:
[conformance/results/README.md](conformance/results/README.md).

## Layout

| Path                                    | What                                                                                                                                        |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/pacod-id`                       | The service: OpenID Connect provider for our own applications, accounts, organisations, role bindings, the authorization feed, mail, audit. |
| `crates/pacod-id-authz`                 | The rules engine: product manifests, scopes, bindings, `allowed`, `has_role`, `touches`.                                                    |
| `crates/pacod-id-jose`                  | Compact JWS (RS256, ES256) on ring, and JWKS.                                                                                               |
| `crates/pacod-id-client`                | What a Rust product needs: token checks, the bindings cache, introspection, token exchange.                                                 |
| `python/`                               | The Python package `pacod_id`: the rules engine, token checks, FastAPI dependencies.                                                        |
| `packages/authz`                        | `@pacod/id-authz`: the rules engine in TypeScript, and helpers generated from a manifest.                                                   |
| `packages/ui`                           | `@pacod/ui`: design tokens, brand themes and the sign-in components.                                                                        |
| `packages/client`, `packages/sveltekit` | `@pacod/id-client`, `@pacod/id-sveltekit`: the BFF sign-in flow for SvelteKit apps.                                                         |
| `ui/`                                   | The hosted sign-in and account pages.                                                                                                       |
| `policy/engine-vectors/`                | YAML test vectors every port of the rules engine must pass.                                                                                 |
| `deploy/`                               | The image (`docker/`), a development stack (`compose/`) and the Helm chart (`helm/pacod-id`).                                               |
| `conformance/`                          | The OpenID Foundation's conformance suite against the image, run locally.                                                                   |

## Development

```sh
make test-services   # the tests' PostgreSQL on 127.0.0.1:25434
make verify          # fmt, clippy, lint, type checks and all tests (Rust, TypeScript, Python)
make e2e             # the hosted pages and the fixture apps in a real browser (Docker)
make image           # the image; make compose-up starts it with PostgreSQL, mail and a mock Google
make measure         # token endpoint and feed latency against the running stack
make chart-check     # the Helm chart: lint, schemas, and its configuration through pacod-id
```

Rust 1.90 or newer, Node 22.17 or newer, Python 3.13 with uv.

## Licence

Apache-2.0.
