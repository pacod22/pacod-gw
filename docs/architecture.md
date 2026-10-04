# Architecture

```
                 ┌───────────────┐
 clients ──────▶ │ pacod-gw-core │ ──────▶ OpenAI / Anthropic / Bedrock / Ollama ...
 (OpenAI API)    │    (Rust)     │
                 └──┬────┬───┬───┘
       UsageEvent   │    │   │ OTLP metrics/traces
(pacod-gw-events)   │    │   └──────────▶ otel-collector ──▶ Prometheus ──▶ Grafana
                    ▼    │ admin API
       ┌────────────────────┐  │
       │ pacod-gw-analytics │  │
       │       (Rust)       │  │
       └───┬────────────┬───┘  │
           ▼            │ reports API
      ClickHouse        ▼      ▼
                    ┌────────────────┐
                    │ ui (SvelteKit) │
                    └────────────────┘
```

## Components

| Component | Path | Role |
|---|---|---|
| **pacod-gw-core** | `crates/pacod-gw-core` | Data plane. Authenticates virtual keys, applies rate limits and quotas, routes to providers with fallbacks, and streams responses. Emits a `UsageEvent` per request and exposes an admin API for the UI. |
| **pacod-gw-events** | `crates/pacod-gw-events` | The shared `UsageEvent` schema. It is the contract between core (producer) and analytics (consumer). The UI mirrors it in `ui/src/lib/types.ts`. |
| **pacod-gw-analytics** | `crates/pacod-gw-analytics` | Tracking, statistics and reporting. Ingests events, stores them in ClickHouse, computes rollups (tokens, cost, latency, errors by tenant, key and model) and serves the reports API. |
| **ui** | `ui/` | SvelteKit admin console covering providers, routes, keys, analytics and reports. |
| **monitoring** | `deploy/` | Operational observability: OpenTelemetry collector, Prometheus and Grafana. This is configuration, not code. |

## Two kinds of telemetry

- **Monitoring** answers "is the gateway healthy right now?" It covers request rate, error rate, latency percentiles and upstream health, uses Prometheus metrics and OTel traces, and is shown in Grafana.
- **Analytics** answers "who used what, and what did it cost?" It is built from per-request usage events stored in ClickHouse, which the analytics service aggregates and serves to the UI and to reports.

## Ports (local)

| Service | Port |
|---|---|
| pacod-gw-core: proxy | 8080 |
| pacod-gw-core: admin API | 8081 |
| pacod-gw-core: metrics | 9100 |
| pacod-gw-analytics | 8090 |
| ui | 3000 (dev server: 5173) |
| Grafana | 3001 |
| Prometheus | 9090 |
| ClickHouse HTTP | 8123 |
| OTel collector | 4317 / 4318 |
