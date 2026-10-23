# pacod-gw-load

The load engine and the load agent behind the console's load tests (Platform → Testing →
Load), and the engine `gw-load` runs on.

The tester (`tester/`) decides what a load test does, sets up the organisation, key and models
it needs, and drives one or more agents. An agent only makes calls and counts them. It reports
each second of its run, and the tester adds up the seconds of all agents. Agents run in
containers of their own, in the stack or on other machines; they need to reach the gateway's
data plane, and the tester needs to reach them.

## The agent

`pacod-gw-load-agent` serves the tester on one port. Every route but `/healthz` needs
`Authorization: Bearer <token>`, the token the tester has as `TESTER_LOAD_TOKEN`.

| Route | What it does |
| --- | --- |
| `GET /healthz` | `{"status":"ok"}`, without a token. |
| `GET /v1/info` | Name, version, default target, caps, CPUs, and the run in progress. |
| `POST /v1/runs` | Prepares a run. The agent checks the plan against its caps, opens `warmup_connections` connections, makes one probe call, and answers 201 with the probe. A probe answered with 401 or 403 ends there (state `failed`): a load of wrong keys would get the agent's address blocked and measure nothing. |
| `POST /v1/runs/{id}/start` | Starts the prepared run: 202. |
| `GET /v1/runs/{id}/report?after=<t>` | The run's state and the seconds after `t`. Seconds up to `t` are dropped, so a report that went missing comes again with the next. |
| `POST /v1/runs/{id}/cancel` | Stops the run at once: 202. |

Errors are `{"error":{"code","message"}}`. The codes are `unauthorized` (401), `busy` (409, one
run at a time), `invalid` (422), `not_found` (404) and `failed`.

**The lease.** Each report renews it. When it runs out, the agent stops the run on its own
(`ended: lease_expired`), and drops a run that was prepared but never started. The tester
asks once a second; `lease_s` is 30 by default. A tester that went away therefore does not
leave load running.

**The target.** A plan without `base_url` goes to `LOAD_AGENT_TARGET_URL`, which is this agent's
route to the gateway: `http://pacod-gw:8080` in the stack, the public API host on another machine.
A plan with `base_url` goes there instead: an external gateway, such as a minimal gateway
(`pacod-gw-lite`). Any http or https URL is accepted, without user name, password, query or
fragment; the console decides, and the run record keeps who aimed where.

**The key.** It comes with the plan and is held in memory for the run only. It is never logged,
and it is replaced by `[redacted]` in a probe's body excerpt.

### Variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `LOAD_AGENT_TOKEN` or `LOAD_AGENT_TOKEN_FILE` | – | The token the tester presents, at least 32 characters. Never a command-line argument. |
| `LOAD_AGENT_TARGET_URL` | – | The gateway as this agent reaches it. |
| `LOAD_AGENT_NAME` | `agent` | The name the console shows. |
| `LOAD_AGENT_LISTEN` | `0.0.0.0:8096` | Where the tester reaches the agent. |
| `LOAD_AGENT_MAX_CONCURRENCY` | 10000 | Calls at a time, at most. |
| `LOAD_AGENT_MAX_RATE` | 5000 | Calls a second, at most (rate mode). |
| `LOAD_AGENT_MAX_DURATION` | 3600 | Seconds a run lasts, at most. |
| `LOAD_AGENT_MAX_BODY_BYTES` | 1048576 | The request body, at most. |
| `LOAD_AGENT_CA_FILE` | – | PEM certificates to trust besides the system's, for a gateway behind a private CA. |
| `LOAD_AGENT_LOG_FORMAT` | `json` | `json` or `text`, in any case; anything else is `json`. `RUST_LOG` sets the level, `SERVICE_NAME` the `service` key (`gw-load-agent`); pacod-demo/docs/LOGGING.md. |

## The engine

A run follows its **stages**. Each stage moves the target in a straight line from where the
previous one ended (0 at the start) to its own target; a stage of 0 seconds jumps. The
**mode** decides what the target means:

- **Concurrency** (closed loop): `target` calls at a time, each started when the previous one
  ended. A slow gateway slows its clients down with it.
- **Rate** (open loop): `target` calls a second, started on time whether or not earlier calls
  have finished. Above `max_in_flight` running calls, an arrival is skipped and counted.

The run ends when its stages are over or `max_calls` are used up. Running calls then get
`drain_s` to finish. A cancel or an expired lease ends them at once. Calls that did not finish
count as `cancelled`, not as errors.

**What a second holds.** Each call that finished in the second is counted once:
- `ok`: a 2xx answer read to its end;
- under its status in `status`: any other answer;
- in `errors`: `connect`, `timeout`, or `body` (the answer broke off).

The second also holds:
- latencies of the successes, as histograms: time to the first body byte, the whole call, and
  the gateway's own `x-gw-delay-ms`;
- body bytes and calls in flight;
- `skipped` arrivals and the scheduler's `lag_ms` (rate mode);
- the agent's CPU use. An agent near its CPUs, or with a growing lag, measures itself.

Calls use HTTP/1.1 unless the plan asks for HTTP/2. Over TLS, HTTP/2 would carry thousands of
calls over a few connections, which is not what most clients do.

## Histograms

Each latency is a sparse histogram in microseconds: `{n, min, max, sum, b: [[index, count], …]}`.
Counts per bucket add up across seconds and agents, which percentiles do not.

The buckets are integer log-linear, so Rust and Python agree on them exactly:
- a value below 128 µs is its own bucket;
- above, each power of two is split into 64 buckets. With `m` the highest set bit,
  `shift = m - 6`, `mantissa = v >> shift`, and the index is
  `128 + (shift - 1) * 64 + (mantissa - 64)`.

A bucket's midpoint is within 0.8% of every value in it. Percentiles are taken by nearest rank
on the midpoints, kept within the exact smallest and largest value.

## Tests

- `cargo test -p pacod-gw-load`: the histogram, the engine against a stub server, and the
  contract.
- `cargo test -p pacod-gw-testkit --test load_agent`: the agent over HTTP against the mock
  provider.
- `tests/contract/*.json`: the contract with the tester. The tester's tests read the same
  files, so a change to either side that breaks the other fails a test.
