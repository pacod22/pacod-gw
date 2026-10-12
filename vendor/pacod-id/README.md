# pacod-id

One place where people and machines sign in, and where it is recorded what they may do, for every
PACOD product: the AI gateway (pacod-gw), PACOD and its consumer brand Vanga.

The design is in [docs/concept.md](docs/concept.md).

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

## Development

```sh
make test-services   # the tests' PostgreSQL on 127.0.0.1:25434
make verify          # fmt, clippy, lint, type checks and all tests (Rust, TypeScript, Python)
```

Rust 1.90 or newer, Node 22.17 or newer, Python 3.13 with uv.

## Licence

Apache-2.0.
