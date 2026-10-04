# pacod-gw

An AI gateway made up of a Rust data plane, an analytics service, a SvelteKit admin UI and an observability stack.

## Layout

```
pacod-gw/
├── Cargo.toml                 # Rust workspace
├── crates/
│   ├── pacod-gw-core/         # proxy: auth, rate limiting, routing, providers, telemetry
│   ├── pacod-gw-events/       # shared UsageEvent schema (core → analytics)
│   └── pacod-gw-analytics/    # tracking, statistics and reporting service + ClickHouse migrations
├── ui/                        # SvelteKit admin console
├── config/                    # gateway.example.toml
├── deploy/                    # docker-compose, Dockerfiles, Prometheus, Grafana, OTel collector
├── docs/                      # architecture.md
└── Makefile
```

See [docs/architecture.md](docs/architecture.md) for how the pieces fit together.

## Getting started

Prerequisites: Rust (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`), Node 22.17+ and Docker.

```sh
cp .env.example .env                              # add provider API keys
cp config/gateway.example.toml config/gateway.toml

make run-core        # gateway on :8080
make run-analytics   # analytics on :8090
make ui-dev          # UI on :5173
make up              # full stack in Docker (Grafana on :3001)
```
