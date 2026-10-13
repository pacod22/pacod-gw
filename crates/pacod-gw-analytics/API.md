# pacod-gw-analytics HTTP API

The analytics service stores the usage events and audit records the gateway core ships, and
answers report queries. The core is its only caller: the UI talks to the core, and the core
forwards (concept section 10.3).

All examples below are real responses for one small data set: nine calls, seven of them made by
organisation `org-a` in March 2026, two of them audited. A test (`tests/it/api_doc.rs`) sends
every example request in this file and compares the answer, so the examples cannot drift from
the service.

- [Conventions](#conventions)
- [Probes](#probes): `/healthz`, `/readyz`
- [Ingest](#ingest): `POST /ingest/usage`, `POST /ingest/audit`
- [Reports](#reports): `overview`, `usage`, `breakdown`, `requests`, `traces`, `end-users`,
  `errors`, `delay`, `accuracy`
- [Audit records](#audit-records): `GET /reports/audit`, `GET /reports/audit/{audit_id}`,
  `DELETE /audit`
- [Erasure](#erasure): `DELETE /erasure`
- [Errors](#errors)
- [Configuration](#configuration)

## Conventions

### Authentication

Every route except `/healthz` and `/readyz` requires the header `x-gw-internal-token` with the
value of `PGW_INTERNAL_TOKEN`. Anything else is answered with `401`, including paths that do
not exist.

### Scope and trust

The service applies the filters it receives and does no authorisation of its own. The core must
replace `organisation_id` and `workspace_id` with the scope the caller is entitled to before it
forwards a query. A query without them covers every organisation.

### Query parameters are checked strictly

A parameter the endpoint does not know is a `400 unknown_parameter`, never ignored. This is
deliberate: a misspelt `organisation_id` must not turn into a report over all organisations.
Forward only the parameters documented for the endpoint.

### Time

- Times are RFC 3339, for example `2026-03-12T10:30:00Z`. A `+` in an offset must be sent as
  `%2B`. Responses are always in UTC with a `Z`.
- A range is half-open: `from <= timestamp < to`.
- Reports over a range default to the last 24 hours: without `to` the range ends now, without
  `from` it starts 24 hours before its end. The response echoes the range it used as `from`
  and `to`.
- Lookups by id (`requests/{id}`, `traces/{trace_id}`, `audit/{audit_id}`) have no default
  range.
- Buckets are aligned to UTC. A week starts on Monday. A bucket is named by its start.

### Common filters

Accepted by every report on usage events unless stated otherwise. All are optional.

| Parameter | Restricts to events whose |
|---|---|
| `organisation_id` | `organisation_id` equals the value |
| `workspace_id` | `workspace_id` equals the value |
| `provider` | `provider` equals the value |
| `model` | `model_served` equals the value |
| `template` | `template` equals the value |
| `key_id` | `key_id` equals the value |
| `end_user` | `end_user` equals the value |
| `status` | `status` equals the value (`ok`, `error`, `timeout`, `cancelled`, `rejected`) |
| `mode` | `mode` equals the value (`proxy`, `managed`) |
| `agent_id` | `agent_id` equals the value |
| `user_id` | `user_id` equals the value. Not the same as `end_user` |
| `task` | `task` equals the value |
| `complexity` | `complexity` equals the value (`low`, `medium`, `high`) |
| `routing_scheme` | `routing_scheme` equals the value: the routing scheme that chose the model |
| `routing_rule` | `routing_rule` equals the value: the scheme's rule that matched, `default` when none did |
| `tag.<key>` | tag `<key>` has the value, for example `tag.purpose=planner` |

- Different filters are combined with AND.
- A filter given several times matches any of its values:
  `workspace_id=ws-1&workspace_id=ws-2`.
- An empty value is a value: `provider=` selects events without a provider, `tag.purpose=`
  events without that tag.
- Values are matched exactly and case-sensitively.

### The standard counters

Every aggregated row carries the same eight fields, called "the counters" below:

| Field | Meaning |
|---|---|
| `requests` | Number of events |
| `errors` | Events whose `status` is not `ok` |
| `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `reasoning_tokens` | Sums of the event fields |
| `cost` | Sum of the costs in the reporting currency: each event's `cost_reporting`, converted when the call settled. An event without one (written before conversion existed, or priced in a currency without an exchange rate) counts with `cost` as recorded; an event without a cost as zero. |

### Groups

Where a report groups, the group value is in the field `group`. An event that has no value
for the grouping column (no provider, no such tag) is in the group `""`. In a report that is
not grouped, `group` is `null`.

`model` as a filter or grouping always means `model_served`.

`agent_id`, `user_id`, `task` and `complexity` are the caller's own attribution of a call, sent
in the `gateway` object or as `x-gw-*` headers. Events from before these fields existed, and
calls that did not send them, are in the group `""`.

`routing_scheme` and `routing_rule` say how a template with several models chose the one that
served: the scheme and the rule that matched. Calls without a template, and templates routed
by priority alone, are in the group `""`. An event also carries `routing_source` (where the
scheme was assigned), `failovers` (how many models the call moved on from) and `models_tried`
(those models, in order).

### Lists of calls

Where a report returns calls, each is a complete usage event exactly as defined by
`pacod_gw_events::UsageEvent` (one flat JSON object; see the example under
[`/reports/requests`](#get-reportsrequests)). Such lists are always called `requests`,
`recent_requests` or `slowest_requests`.

### Paging

`/reports/requests` and `/reports/audit` return `next_cursor`. Pass it back as `cursor`
(URL-encoded) with otherwise unchanged parameters to get the next page. `null` means there is
no further page. A cursor is opaque.

### Percentiles

Percentiles of `gateway_delay_ms` are exact, not estimated. They use the nearest rank without
interpolation: the value at position `floor(level × n)` (counting from zero) of the `n`
sorted values. The median of two values is therefore the larger one.

### CSV

`format=csv` on every report returns `text/csv; charset=utf-8` as an attachment, RFC 4180,
lines ending in CRLF, with a header line that is present even when there are no rows. Where
the JSON answer is nested, the CSV holds one table of it:

| Report | One line per | File name |
|---|---|---|
| `overview` | period (`hour`, `day`, `week`, `month`): its `from`, `to` and totals | `overview.csv` |
| `usage` | group and bucket | `usage.csv` |
| `breakdown` | group | `breakdown.csv` |
| `ranking` | ranked row | `ranking.csv` |
| `requests`, `traces/{trace_id}` | call, every event field | `requests.csv`, `trace.csv` |
| `end-users` | end user | `end-users.csv` |
| `errors` | provider and model with failures (`by_provider_model`) | `errors.csv` |
| `delay` | group and bucket: the points of the series, with `group` | `delay.csv` |
| `accuracy` | the one row of figures | `accuracy.csv` |
 Maps and lists (`tags`, `dropped_params`) are written as JSON text. Cells are not
altered to defuse spreadsheet formulas; a consumer that opens them in a spreadsheet should
import the columns as text.

### Duplicates

Delivery from the core is at least once. Every report removes duplicates when it reads, so an
event counts once however often it was delivered.

### Rollups

The overview, `breakdown` by `organisation_id`, `workspace_id`, `provider`, `model`,
`template` or `key_id`, and `end-users` read hourly and daily totals for the hours and days
the rollup job has built, when their filters are among the rollups' columns, and the events
for everything else. The answers are those the events give; a cost may differ in the last
digits, because it is added up in another order. An hour that receives events after it closed
is read from the events until the job has built it again, within five minutes.

## Probes

### `GET /healthz`

No token. `200` whenever the process is up.

```json
{"status":"ok"}
```

### `GET /readyz`

No token. `200` only when ClickHouse answers and the schema is at the version this build
expects.

```json
{"status":"ready","schema_version":2}
```

Otherwise `503`:

```json
{"status":"unavailable","reason":"clickhouse_unreachable"}
```

`reason` is `clickhouse_unreachable` (no answer), `migrations_pending` (ClickHouse answers,
the schema is missing or older) or `clickhouse_error` (ClickHouse refused, for example a wrong
password).

The service starts listening before ClickHouse is reachable and retries the migrations in the
background. Until they have succeeded, every route behind the token answers `503 not_ready`.

## Ingest

### `POST /ingest/usage` and `POST /ingest/audit`

The paths, header names and reply type are the constants in `pacod_gw_events::ingest`.

| | |
|---|---|
| Header `x-gw-batch-id` | Required. 1 to 200 printable ASCII characters without spaces. |
| Header `content-type` | `application/x-ndjson` (not checked). |
| Body | Newline-delimited JSON: one `UsageEvent` (or `AuditRecord`) per line. Blank lines are skipped. Not compressed. At most `ANALYTICS_MAX_BATCH_BYTES` (default 64 MiB), for both kinds of batch. |

`200` is sent only after ClickHouse has written the batch:

```json
{"batch_id":"i-1:000001","rows":9}
```

`rows` is the number of records in the batch, also when the batch was a repeat.

| Status | `error.code` | Meaning | What the core should do |
|---|---|---|---|
| `200` | | Stored. | Advance the checkpoint. |
| `400` | `malformed_line` | A line is not a valid record. `error.line` is its number, counted from one, blank lines included. Nothing of the batch was stored. | Do not retry the batch unchanged. |
| `400` | `invalid_batch_id` | The batch id header is missing or malformed. | Fix the request. |
| `413` | `batch_too_large` | The body exceeds the size limit. | Send smaller batches. |
| `415` | `unsupported_encoding` | The body has a `content-encoding`. | Send it uncompressed. |
| `503` | `storage_unavailable` | ClickHouse did not accept the insert. | Retry the same batch with the same id. |
| `503` | `not_ready` | The schema is not applied yet. | Retry the same batch with the same id. |

```json
{"error":{"code":"malformed_line","message":"line 2: missing field `id` at line 1 column 36","line":2}}
```

**The batch id is the deduplication token.** It is passed to ClickHouse as
`insert_deduplication_token`, per table:

- The same batch sent again under the same id is dropped by ClickHouse; the stored rows do not
  change. ClickHouse remembers the last 10 000 inserted blocks per table; a batch is one block
  for every calendar month its records fall into.
- A batch id must never be reused for different content within that window: ClickHouse would
  drop the second batch and the service would still answer `200`.
- An event or audit record that arrives again under a different batch id (or after the window)
  is stored a second time and removed when read, by its key: `(organisation_id, workspace_id,
  timestamp, id)` for events, `(workspace_id, session_id, timestamp, audit_id)` for audit
  records. A record that is sent again must therefore keep those fields. Where the copies
  differ in other fields, the copy inserted last is the one reported.

A usage event is stored field by field; floats keep their exact bits and the time stamp its
nanoseconds. Time stamps must lie between the years 1900 and 2262. A field unknown to this
build of the service is dropped (the core is newer); the service never invents one.

## Reports

All `GET`. All return `200` with a JSON object, or an [error](#errors).

### `GET /reports/overview`

The counters for the hour, day, week and month that contain `to`, each from its start up to
`to`.

| Parameter | Default | |
|---|---|---|
| `to` | now | The instant the periods end at. `from` is not accepted. |
| common filters | | |

`GET /reports/overview?to=2026-03-12T10:30:00Z&organisation_id=org-a`

```json
{
  "to": "2026-03-12T10:30:00Z",
  "periods": {
    "hour": {
      "from": "2026-03-12T10:00:00Z",
      "requests": 3,
      "errors": 2,
      "input_tokens": 120,
      "output_tokens": 50,
      "cache_read_tokens": 10,
      "cache_write_tokens": 5,
      "reasoning_tokens": 0,
      "cost": 0.5
    },
    "day": {
      "from": "2026-03-12T00:00:00Z",
      "requests": 5,
      "errors": 2,
      "input_tokens": 720,
      "output_tokens": 350,
      "cache_read_tokens": 10,
      "cache_write_tokens": 5,
      "reasoning_tokens": 40,
      "cost": 1.5
    },
    "week": {
      "from": "2026-03-09T00:00:00Z",
      "requests": 6,
      "errors": 3,
      "input_tokens": 1020,
      "output_tokens": 350,
      "cache_read_tokens": 10,
      "cache_write_tokens": 5,
      "reasoning_tokens": 40,
      "cost": 1.75
    },
    "month": {
      "from": "2026-03-01T00:00:00Z",
      "requests": 7,
      "errors": 3,
      "input_tokens": 2020,
      "output_tokens": 850,
      "cache_read_tokens": 110,
      "cache_write_tokens": 55,
      "reasoning_tokens": 240,
      "cost": 3.75
    }
  }
}
```

### `GET /reports/usage`

A time series per group.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `bucket` | `hour` | `hour`, `day`, `week`, `month` |
| `group_by` | `none` | `none`, `provider`, `model`, `template`, `key_id`, `end_user`, `status`, `mode`, `agent_id`, `user_id`, `task`, `complexity`, `routing_scheme`, `routing_rule`, `tag:<key>` |
| `format` | `json` | `json`, `csv` |
| common filters | | |

Each point has the counters plus `gateway_delay_p50_ms` and `gateway_delay_p99_ms`. Series are
ordered by group, points by time. A bucket without events is not in the series. The number of
series and points is not capped: choose the range, bucket and grouping accordingly.

`GET /reports/usage?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&bucket=day&group_by=provider`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "bucket": "day",
  "group_by": "provider",
  "series": [
    {
      "group": "",
      "points": [
        {
          "bucket": "2026-03-12T00:00:00Z",
          "requests": 1,
          "errors": 1,
          "input_tokens": 0,
          "output_tokens": 0,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0,
          "reasoning_tokens": 0,
          "cost": 0.0,
          "gateway_delay_p50_ms": 0.5,
          "gateway_delay_p99_ms": 0.5
        }
      ]
    },
    {
      "group": "anthropic",
      "points": [
        {
          "bucket": "2026-03-03T00:00:00Z",
          "requests": 1,
          "errors": 0,
          "input_tokens": 1000,
          "output_tokens": 500,
          "cache_read_tokens": 100,
          "cache_write_tokens": 50,
          "reasoning_tokens": 200,
          "cost": 2.0,
          "gateway_delay_p50_ms": 3.0,
          "gateway_delay_p99_ms": 3.0
        },
        {
          "bucket": "2026-03-12T00:00:00Z",
          "requests": 2,
          "errors": 1,
          "input_tokens": 120,
          "output_tokens": 50,
          "cache_read_tokens": 10,
          "cache_write_tokens": 5,
          "reasoning_tokens": 0,
          "cost": 0.5,
          "gateway_delay_p50_ms": 4.0,
          "gateway_delay_p99_ms": 4.0
        }
      ]
    },
    {
      "group": "openai",
      "points": [
        {
          "bucket": "2026-03-10T00:00:00Z",
          "requests": 1,
          "errors": 1,
          "input_tokens": 300,
          "output_tokens": 0,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0,
          "reasoning_tokens": 0,
          "cost": 0.25,
          "gateway_delay_p50_ms": 8.0,
          "gateway_delay_p99_ms": 8.0
        },
        {
          "bucket": "2026-03-12T00:00:00Z",
          "requests": 2,
          "errors": 0,
          "input_tokens": 600,
          "output_tokens": 300,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0,
          "reasoning_tokens": 40,
          "cost": 1.0,
          "gateway_delay_p50_ms": 5.0,
          "gateway_delay_p99_ms": 5.0
        }
      ]
    }
  ]
}
```

With `group_by=none` there is one series with `"group": null` (or no series when there are no
events).

With `format=csv` (file name `usage.csv`), one line per point:

`GET /reports/usage?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&bucket=day&group_by=provider&format=csv`

```csv
bucket,group,requests,errors,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,cost,gateway_delay_p50_ms,gateway_delay_p99_ms
2026-03-12T00:00:00Z,,1,1,0,0,0,0,0,0.0,0.5,0.5
2026-03-03T00:00:00Z,anthropic,1,0,1000,500,100,50,200,2.0,3.0,3.0
2026-03-12T00:00:00Z,anthropic,2,1,120,50,10,5,0,0.5,4.0,4.0
2026-03-10T00:00:00Z,openai,1,1,300,0,0,0,0,0.25,8.0,8.0
2026-03-12T00:00:00Z,openai,2,0,600,300,0,0,40,1.0,5.0,5.0
```

### `GET /reports/breakdown`

Totals per group, largest cost first. At equal cost the group with more requests comes first,
then the smaller name.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `by` | required | `organisation_id`, `workspace_id`, `provider`, `model`, `template`, `key_id`, `end_user`, `error_type`, `agent_id`, `user_id`, `task`, `complexity`, `routing_scheme`, `routing_rule`, `tag:<key>` |
| `limit` | 50 | 1 to 1000: the number of groups |
| `format` | `json` | `json`, `csv` |
| common filters | | |

`GET /reports/breakdown?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&by=model&limit=3`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "by": "model",
  "limit": 3,
  "rows": [
    {
      "group": "claude-sonnet",
      "requests": 1,
      "errors": 0,
      "input_tokens": 1000,
      "output_tokens": 500,
      "cache_read_tokens": 100,
      "cache_write_tokens": 50,
      "reasoning_tokens": 200,
      "cost": 2.0
    },
    {
      "group": "gpt-mini",
      "requests": 3,
      "errors": 1,
      "input_tokens": 900,
      "output_tokens": 300,
      "cache_read_tokens": 0,
      "cache_write_tokens": 0,
      "reasoning_tokens": 40,
      "cost": 1.25
    },
    {
      "group": "claude-haiku",
      "requests": 2,
      "errors": 1,
      "input_tokens": 120,
      "output_tokens": 50,
      "cache_read_tokens": 10,
      "cache_write_tokens": 5,
      "reasoning_tokens": 0,
      "cost": 0.5
    }
  ]
}
```

`by=error_type` includes the successful calls as the group `""`; add `status` filters to leave
them out.

With `format=csv` (file name `breakdown.csv`): `group` followed by the counters.

`GET /reports/breakdown?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&by=model&limit=2&format=csv`

```csv
group,requests,errors,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,cost
claude-sonnet,1,0,1000,500,100,50,200,2.0
gpt-mini,3,1,900,300,0,0,40,1.25
```

### `GET /reports/last-used`

When each group was last called, and how often in the range, most recent first. For "last
used" next to templates, keys and models.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `by` | required | as for `/reports/breakdown` |
| `limit` | 200 | 1 to 1000: the number of groups |
| common filters | | |

`GET /reports/last-used?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&by=model&limit=2`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "by": "model",
  "rows": [
    { "group": "", "last_used": "2026-03-12T10:25:00Z", "requests": 1 },
    { "group": "claude-haiku", "last_used": "2026-03-12T10:20:00Z", "requests": 2 }
  ]
}
```

### `GET /reports/requests`

The request log: events newest first.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `limit` | 50 | 1 to 1000 |
| `cursor` | | `next_cursor` of the previous page |
| `trace_id` | | Events of this trace |
| `session_id` | | Events of this session |
| `audited` | | `true`: only events with an audit record. `false`: only events without. |
| `format` | `json` | `json`, `csv` |
| common filters | | |

`GET /reports/requests?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&end_user=alice&limit=1`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "limit": 1,
  "requests": [
    {
      "id": "00000000-0000-0000-0000-000000000002",
      "timestamp": "2026-03-12T10:20:00Z",
      "instance_id": "i-1",
      "organisation_id": "org-a",
      "workspace_id": "ws-1",
      "key_id": "key-1",
      "end_user": "alice",
      "session_id": "sess-1",
      "trace_id": "trace-1",
      "tags": {
        "purpose": "plan"
      },
      "agent_id": "planner-bot",
      "user_id": "u-1",
      "task": "plan",
      "complexity": "high",
      "mode": "managed",
      "interface": "openai",
      "provider": "anthropic",
      "model_requested": "claude-haiku",
      "model_served": "claude-haiku",
      "model_reported": "",
      "template": "planner",
      "template_version": 0,
      "path": "",
      "routing_scheme": "",
      "routing_scheme_version": 0,
      "routing_source": "",
      "routing_rule": "",
      "failovers": 0,
      "models_tried": [],
      "status": "error",
      "http_status": 500,
      "error_type": "rate_limited",
      "error_message": "",
      "finish_reason": "",
      "attempts": 1,
      "stream": false,
      "upstream_request_id": "",
      "gateway_delay_ms": 4.0,
      "receive_ms": 1.0,
      "pre_ms": 4.0,
      "connect_ms": 0.0,
      "retry_ms": 0.0,
      "upstream_first_byte_ms": 0.0,
      "upstream_stream_ms": 0.0,
      "post_ms": 0.0,
      "relay_max_ms": 0.0,
      "settle_ms": 0.0,
      "total_ms": 250.0,
      "input_tokens": 20,
      "cache_read_tokens": 0,
      "cache_write_tokens": 0,
      "cache_write_5m_tokens": 0,
      "cache_write_1h_tokens": 0,
      "output_tokens": 0,
      "reasoning_tokens": 0,
      "characters": 0,
      "server_tool_calls": 0,
      "usage_source": "missing",
      "cost": null,
      "currency": "USD",
      "cost_reporting": null,
      "reporting_currency": "",
      "price_version": 0,
      "price_variant": "",
      "price_missing": false,
      "dropped_params": [],
      "audit_id": ""
    }
  ],
  "next_cursor": "1773310800000000000:00000000-0000-0000-0000-000000000002"
}
```

With `format=csv` (file name `requests.csv`): one column per event field, in the order above.
The cursor of the next page is in the response header `x-next-cursor`; the header is absent
on the last page.

### `GET /reports/requests/{id}`

One event by its id (a UUID). Returns the event object itself, or `404 not_found`.

| Parameter | |
|---|---|
| `organisation_id`, `workspace_id` | The only filters accepted. An event outside the scope is a `404`. |
| `from`, `to` | Optional. No default: without them the event is found whatever its age. Giving them makes the lookup cheaper. |

`GET /reports/requests/00000000-0000-0000-0000-000000000002?organisation_id=org-a` returns the
object shown inside `requests` above.

### `GET /reports/traces/{trace_id}`

The calls of one trace in time order, with their totals. `404 not_found` when the trace has no
calls in the scope.

| Parameter | |
|---|---|
| `organisation_id`, `workspace_id` | The only filters accepted. |
| `from`, `to` | Optional. No default. |

`totals` has the counters plus `gateway_delay_ms` (the sum over the calls) and the time stamps
of the first and last call. `totals` covers every call of the trace; `requests` holds at most
the first 1000.

`GET /reports/traces/trace-1`

```json
{
  "trace_id": "trace-1",
  "totals": {
    "requests": 2,
    "errors": 1,
    "input_tokens": 120,
    "output_tokens": 50,
    "cache_read_tokens": 10,
    "cache_write_tokens": 5,
    "reasoning_tokens": 0,
    "cost": 0.5,
    "gateway_delay_ms": 6.0,
    "first_request_at": "2026-03-12T10:05:00Z",
    "last_request_at": "2026-03-12T10:20:00Z"
  },
  "requests": [
    { "id": "00000000-0000-0000-0000-000000000001", "…": "the complete usage event" },
    { "id": "00000000-0000-0000-0000-000000000002", "…": "the complete usage event" }
  ]
}
```

### `GET /reports/end-users`

One row per end user, largest cost first (then more requests, then name). Events without an
end user are not listed.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `limit` | 100 | 1 to 1000 |
| `offset` | 0 | Rows to skip |
| common filters | | |

`GET /reports/end-users?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&limit=2`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "limit": 2,
  "offset": 0,
  "rows": [
    {
      "end_user": "alice",
      "requests": 3,
      "errors": 1,
      "input_tokens": 1120,
      "output_tokens": 550,
      "cache_read_tokens": 110,
      "cache_write_tokens": 55,
      "reasoning_tokens": 200,
      "cost": 2.5,
      "first_request_at": "2026-03-03T12:00:00Z",
      "last_request_at": "2026-03-12T10:20:00Z"
    },
    {
      "end_user": "bob",
      "requests": 2,
      "errors": 1,
      "input_tokens": 500,
      "output_tokens": 100,
      "cache_read_tokens": 0,
      "cache_write_tokens": 0,
      "reasoning_tokens": 40,
      "cost": 1.25,
      "first_request_at": "2026-03-10T15:00:00Z",
      "last_request_at": "2026-03-12T08:00:00Z"
    }
  ]
}
```

`first_request_at` and `last_request_at` are within the range queried.

### `GET /reports/end-users/{id}`

One end user: totals, the split by model (largest cost first) and the most recent calls. The
id is one percent-encoded path segment. An end user without events in the range is answered
with zero totals and empty lists, not with a `404`.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `limit` | 20 | 1 to 1000: the number of recent calls |
| common filters | | |

`GET /reports/end-users/alice?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&limit=1`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "end_user": "alice",
  "totals": {
    "requests": 3,
    "errors": 1,
    "input_tokens": 1120,
    "output_tokens": 550,
    "cache_read_tokens": 110,
    "cache_write_tokens": 55,
    "reasoning_tokens": 200,
    "cost": 2.5
  },
  "by_model": [
    {
      "model": "claude-sonnet",
      "requests": 1,
      "errors": 0,
      "input_tokens": 1000,
      "output_tokens": 500,
      "cache_read_tokens": 100,
      "cache_write_tokens": 50,
      "reasoning_tokens": 200,
      "cost": 2.0
    },
    {
      "model": "claude-haiku",
      "requests": 2,
      "errors": 1,
      "input_tokens": 120,
      "output_tokens": 50,
      "cache_read_tokens": 10,
      "cache_write_tokens": 5,
      "reasoning_tokens": 0,
      "cost": 0.5
    }
  ],
  "recent_requests": [
    { "id": "00000000-0000-0000-0000-000000000002", "…": "the complete usage event" }
  ]
}
```

### `GET /reports/errors`

Failures: events whose `status` is not `ok`.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `bucket` | `hour` | `hour`, `day`, `week`, `month`: the width of `over_time` |
| `limit` | 20 | 1 to 1000: the number of recent failures |
| common filters | | |

- `totals`: all events and the failures among them. `error_rate` is `errors / requests`, zero
  without requests.
- `by_type`: failures per `error_type`, most frequent first.
- `by_provider_model`: every provider and model pair with at least one failure, with all its
  requests, most failures first.
- `over_time`: every bucket that has events, in time order.
- `recent_requests`: the newest failures.

`GET /reports/errors?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&bucket=day&limit=1`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "bucket": "day",
  "totals": {
    "requests": 7,
    "errors": 3,
    "error_rate": 0.42857142857142855
  },
  "by_type": [
    {
      "error_type": "quota_exceeded",
      "errors": 1,
      "last_seen_at": "2026-03-12T10:25:00Z"
    },
    {
      "error_type": "rate_limited",
      "errors": 1,
      "last_seen_at": "2026-03-12T10:20:00Z"
    },
    {
      "error_type": "upstream_timeout",
      "errors": 1,
      "last_seen_at": "2026-03-10T15:00:00Z"
    }
  ],
  "by_provider_model": [
    {
      "provider": "",
      "model": "",
      "requests": 1,
      "errors": 1,
      "error_rate": 1.0
    },
    {
      "provider": "anthropic",
      "model": "claude-haiku",
      "requests": 2,
      "errors": 1,
      "error_rate": 0.5
    },
    {
      "provider": "openai",
      "model": "gpt-mini",
      "requests": 3,
      "errors": 1,
      "error_rate": 0.3333333333333333
    }
  ],
  "over_time": [
    {
      "bucket": "2026-03-03T00:00:00Z",
      "requests": 1,
      "errors": 0
    },
    {
      "bucket": "2026-03-10T00:00:00Z",
      "requests": 1,
      "errors": 1
    },
    {
      "bucket": "2026-03-12T00:00:00Z",
      "requests": 5,
      "errors": 2
    }
  ],
  "recent_requests": [
    { "id": "00000000-0000-0000-0000-000000000009", "…": "the complete usage event" }
  ]
}
```

### `GET /reports/delay`

The gateway delay (concept section 14.2).

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `bucket` | `hour` | `hour`, `day`, `week`, `month` |
| `group_by` | `none` | `none`, `mode`, `provider`, `model`, `template`, `workspace_id`, `instance_id`, `agent_id`, `user_id`, `task`, `complexity` |
| `limit` | 20 | 1 to 1000: the number of slowest calls |
| `stage` | | One of `receive`, `pre`, `connect`, `retry`, `upstream_first_byte`, `upstream_stream`, `post`, `relay_max`, `settle`, `total`: the points also carry that stage's percentiles |
| common filters | | |

- `summary`: median, 95th and 99th percentile and maximum of `gateway_delay_ms` over the whole
  range. The four figures are `null` when there are no events.
- `stage_averages`: the average of every timing field of the events, in milliseconds. Each is
  `null` when there are no events.
- `series`: the same four figures per group and bucket, ordered by group, then time. With
  `group_by=none` there is one series with `"group": null`. With `stage`, the response names
  it in `stage`, and every point also has `stage_p50_ms`, `stage_p95_ms` and `stage_p99_ms`
  of the column `<stage>_ms`.
- `slowest_requests`: the calls with the largest `gateway_delay_ms`, largest first.

`GET /reports/delay?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&bucket=day&group_by=mode&limit=1`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "bucket": "day",
  "group_by": "mode",
  "summary": {
    "requests": 7,
    "gateway_delay_p50_ms": 3.0,
    "gateway_delay_p95_ms": 8.0,
    "gateway_delay_p99_ms": 8.0,
    "gateway_delay_max_ms": 8.0
  },
  "stage_averages": {
    "gateway_delay_ms": 3.357142857142857,
    "receive_ms": 1.0,
    "pre_ms": 3.357142857142857,
    "connect_ms": 0.0,
    "retry_ms": 0.0,
    "upstream_first_byte_ms": 0.0,
    "upstream_stream_ms": 0.0,
    "post_ms": 0.0,
    "relay_max_ms": 0.0,
    "settle_ms": 0.0,
    "total_ms": 250.0
  },
  "series": [
    {
      "group": "managed",
      "points": [
        {
          "bucket": "2026-03-03T00:00:00Z",
          "requests": 1,
          "gateway_delay_p50_ms": 3.0,
          "gateway_delay_p95_ms": 3.0,
          "gateway_delay_p99_ms": 3.0,
          "gateway_delay_max_ms": 3.0
        },
        {
          "bucket": "2026-03-12T00:00:00Z",
          "requests": 3,
          "gateway_delay_p50_ms": 2.0,
          "gateway_delay_p95_ms": 4.0,
          "gateway_delay_p99_ms": 4.0,
          "gateway_delay_max_ms": 4.0
        }
      ]
    },
    {
      "group": "proxy",
      "points": [
        {
          "bucket": "2026-03-10T00:00:00Z",
          "requests": 1,
          "gateway_delay_p50_ms": 8.0,
          "gateway_delay_p95_ms": 8.0,
          "gateway_delay_p99_ms": 8.0,
          "gateway_delay_max_ms": 8.0
        },
        {
          "bucket": "2026-03-12T00:00:00Z",
          "requests": 2,
          "gateway_delay_p50_ms": 5.0,
          "gateway_delay_p95_ms": 5.0,
          "gateway_delay_p99_ms": 5.0,
          "gateway_delay_max_ms": 5.0
        }
      ]
    }
  ],
  "slowest_requests": [
    { "id": "00000000-0000-0000-0000-000000000004", "…": "the complete usage event" }
  ]
}
```

### `GET /reports/accuracy`

How much of the spend rests on token counts that did not come from the provider.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| common filters | | |

| Field | Meaning |
|---|---|
| `requests` | All events |
| `total_cost` | Sum of `cost` |
| `non_provider_requests` | Events whose `usage_source` is not `provider` |
| `non_provider_cost` | Their cost |
| `non_provider_share` | `non_provider_cost / total_cost`, zero when there is no cost |
| `price_missing_requests` | Events with `price_missing` set |

`GET /reports/accuracy?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "requests": 7,
  "total_cost": 3.75,
  "non_provider_requests": 3,
  "non_provider_cost": 0.25,
  "non_provider_share": 0.06666666666666667,
  "price_missing_requests": 1
}
```

### `GET /reports/ranking`

Providers or models compared on how they serve: each metric ranked on its own, and one
overall score that weighs the metric scores by a preset.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | |
| `by` | `model` | `model` (one row per provider and model), `provider` |
| `kind` | `chat` | `chat`, `speech`: only calls of one kind are compared |
| `preset` | `balanced` | `balanced`, `speed`, `cost`, `reliability` |
| `min_requests` | 20 | 1 to 100000: the calls a group needs to be ranked |
| `format` | `json` | `json`, `csv` |
| common filters | | |

**Which calls count.** The kind is read from the call's path: chat is `/v1/chat/completions`,
`/v1/messages`, `/gw/v1/chat`, a relayed path ending in `/chat/completions`, `/messages`,
`/responses`, `/api/chat`, `/api/generate`, `:generateContent` or `:streamGenerateContent`, and
events without a path. Speech is `/v1/audio/speech`, `/gw/v1/speech` and relayed paths ending
in `/audio/speech`. Token counting is neither. Of those, a call counts when it reached the
provider: `attempts > 0` and `status` neither `rejected` nor `cancelled`. `requests` counts all
calls of the kind, `eligible` the ones that count. Groups without an eligible call are left out.

**Metrics.** Each has a `value`, a `rank` and a `score`.

| Metric | Value | Better |
|---|---|---|
| `success_rate` | Calls with `status` `ok`, of the eligible ones | higher |
| `first_byte_p50_ms`, `first_byte_p95_ms` | Percentiles (nearest rank, see above) of `upstream_first_byte_ms` of successful calls that measured one. This is the time to the provider's response head, not to the first token. | lower |
| `output_ms_per_token` | `upstream_stream_ms` per output token over successful streamed calls. Chat only. | lower |
| `cost` | Chat: cost per million tokens (input, cache and output). Speech: cost per call. Only calls with a price count, and only when they share one currency: the reporting currency for calls whose cost was converted, the price's otherwise. | lower |

**Ranking.** A group with at least `min_requests` eligible calls is ranked (`ranked: true`).
Per metric, the ranked groups that have a value get competition ranks: equal values share a
rank, and the next rank skips. Costs are ranked only in the response's `currency`, the
currency with the most priced calls among the ranked groups. A group priced in another
currency shows its cost without a rank. The score of a rank is `100 × (n − rank) / (n − 1)`
for `n` ranked values, 100 for a lone value. The overall `score` is the weighted mean of the
metric scores the group has, with the preset's `weights`; a metric without a score drops out
of both sums. Scores are rounded to two decimals. Rows come best score first, then more
eligible calls, then provider and model by name; `rank` is the position. Groups with too few
calls follow, most eligible calls first, with `rank`, `score` and the metric ranks `null`.

| Preset | `success_rate` | `first_byte_p50_ms` | `first_byte_p95_ms` | `output_ms_per_token` | `cost` |
|---|---|---|---|---|---|
| `balanced` | 0.30 | 0.15 | 0.15 | 0.15 | 0.25 |
| `speed` | 0.15 | 0.30 | 0.20 | 0.30 | 0.05 |
| `cost` | 0.20 | 0.05 | 0.05 | 0.05 | 0.65 |
| `reliability` | 0.60 | 0.05 | 0.20 | 0.05 | 0.10 |

**`info`** holds figures that are shown but not ranked. Shares are of the eligible calls.

| Field | Meaning |
|---|---|
| `rate_limited_share` | Calls answered with HTTP 429 |
| `timeout_share` | Calls with `status` `timeout` |
| `cache_read_share` | Cache reads of all input tokens; `null` without input |
| `price_coverage` | Calls with a price |
| `estimated_share` | Calls whose `usage_source` is not `provider` |
| `avg_attempts` | Attempts per call |
| `cost_per_request` | Cost per priced call |
| `output_tokens_per_second` | `1000 / output_ms_per_token` |
| `currency`, `mixed_currencies` | The currency of the priced calls; `null` without any, or when they mix currencies (then `mixed_currencies` is `true`) |

Output per token is measured from the provider's first byte to its last, so on a stream it also
holds any wait between the response head and the first token.

Known limits: with failover, a call is counted for the model that finally served it. The
models it moved on from appear only by name in `models_tried`, so their failures are not
counted against them here.

`GET /reports/ranking?from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a&by=provider&min_requests=2`

```json
{
  "from": "2026-03-01T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "kind": "chat",
  "by": "provider",
  "preset": "balanced",
  "min_requests": 2,
  "currency": "USD",
  "weights": {
    "success_rate": 0.3,
    "first_byte_p50_ms": 0.15,
    "first_byte_p95_ms": 0.15,
    "output_ms_per_token": 0.15,
    "cost": 0.25
  },
  "rows": [
    {
      "rank": 1,
      "score": 100.0,
      "ranked": true,
      "provider": "anthropic",
      "model": null,
      "requests": 3,
      "eligible": 3,
      "metrics": {
        "success_rate": { "value": 0.6666666666666666, "rank": 1, "score": 100.0 },
        "first_byte_p50_ms": { "value": null, "rank": null, "score": null },
        "first_byte_p95_ms": { "value": null, "rank": null, "score": null },
        "output_ms_per_token": { "value": null, "rank": null, "score": null },
        "cost": { "value": 1377.4104683195592, "rank": 1, "score": 100.0 }
      },
      "info": {
        "rate_limited_share": 0.0,
        "timeout_share": 0.0,
        "cache_read_share": 0.08943089430894309,
        "price_coverage": 0.6666666666666666,
        "estimated_share": 0.3333333333333333,
        "avg_attempts": 1.0,
        "cost_per_request": 1.25,
        "output_tokens_per_second": null,
        "currency": "USD",
        "mixed_currencies": false
      }
    },
    {
      "rank": 2,
      "score": 54.55,
      "ranked": true,
      "provider": "openai",
      "model": null,
      "requests": 3,
      "eligible": 3,
      "metrics": {
        "success_rate": { "value": 0.6666666666666666, "rank": 1, "score": 100.0 },
        "first_byte_p50_ms": { "value": null, "rank": null, "score": null },
        "first_byte_p95_ms": { "value": null, "rank": null, "score": null },
        "output_ms_per_token": { "value": null, "rank": null, "score": null },
        "cost": { "value": 2083.3333333333335, "rank": 2, "score": 0.0 }
      },
      "info": {
        "rate_limited_share": 0.0,
        "timeout_share": 0.3333333333333333,
        "cache_read_share": 0.0,
        "price_coverage": 0.6666666666666666,
        "estimated_share": 0.3333333333333333,
        "avg_attempts": 1.0,
        "cost_per_request": 0.625,
        "output_tokens_per_second": null,
        "currency": "USD",
        "mixed_currencies": false
      }
    }
  ]
}
```

With `format=csv` (file name `ranking.csv`), one line per row: `rank`, `score`, `ranked`,
`provider`, `model`, `requests`, `eligible`, then each metric's value and `<metric>_rank`,
`currency` and the other `info` fields.

## Audit records

A record is served until its `expires_at`. After that it is a `404` and absent from the list,
and ClickHouse deletes it in the background.

### `GET /reports/audit/{audit_id}`

The complete record, exactly as defined by `pacod_gw_events::AuditRecord`, or `404 not_found`.
The id is one percent-encoded path segment.

| Parameter | |
|---|---|
| `organisation_id`, `workspace_id` | The only parameters accepted. A record outside the scope is a `404`. |

`GET /reports/audit/aud-1`

```json
{
  "audit_id": "aud-1",
  "event_id": "00000000-0000-0000-0000-000000000001",
  "timestamp": "2026-03-12T10:05:00Z",
  "expires_at": "2099-01-01T00:00:00Z",
  "organisation_id": "org-a",
  "workspace_id": "ws-1",
  "key_id": "key-1",
  "end_user": "alice",
  "session_id": "sess-1",
  "trace_id": "trace-1",
  "agent_id": "planner-bot",
  "user_id": "u-1",
  "task": "plan",
  "complexity": "high",
  "template": "planner",
  "model": "claude-haiku",
  "mode": "managed",
  "client_request": {
    "method": "POST",
    "url": "/v1/chat/completions",
    "status": 0,
    "headers": [
      [
        "content-type",
        "application/json"
      ]
    ],
    "body": "{\"model\":\"@planner\",\"messages\":[{\"role\":\"user\",\"content\":\"Plan the next step.\"}]}",
    "body_encoding": "utf8",
    "body_bytes": 81,
    "same_as": "",
    "chunks": [],
    "truncated": false
  },
  "provider_requests": [
    {
      "method": "POST",
      "url": "https://api.anthropic.com/v1/messages",
      "status": 0,
      "headers": [],
      "body": "{\"model\":\"claude-haiku\"}",
      "body_encoding": "utf8",
      "body_bytes": 24,
      "same_as": "",
      "chunks": [],
      "truncated": false
    }
  ],
  "provider_responses": [
    {
      "method": "",
      "url": "",
      "status": 200,
      "headers": [],
      "body": "{\"content\":\"ok\"}",
      "body_encoding": "utf8",
      "body_bytes": 16,
      "same_as": "",
      "chunks": [],
      "truncated": false
    }
  ],
  "client_response": {
    "method": "",
    "url": "",
    "status": 200,
    "headers": [],
    "body": "",
    "body_encoding": "ref",
    "body_bytes": 16,
    "same_as": "provider_responses[0]",
    "chunks": [],
    "truncated": false
  },
  "resolution": {
    "template": "planner",
    "version": 3
  },
  "timeline": {
    "pre_us": 900
  },
  "outcome": {
    "finish_reason": "stop"
  },
  "truncated": false
}
```

### `GET /reports/audit`

Audit records without their bodies, newest first.

| Parameter | Default | |
|---|---|---|
| `from`, `to` | last 24 hours | Applied to the record's `timestamp` |
| `limit` | 50 | 1 to 1000 |
| `cursor` | | `next_cursor` of the previous page |
| `organisation_id`, `workspace_id`, `model`, `template`, `key_id`, `end_user`, `mode`, `session_id`, `trace_id`, `agent_id`, `user_id`, `task`, `complexity` | | Equality filters, with the rules of the common filters. `model` is the record's `model`. |

Only the fields an audit record carries can be filtered on. `provider`, `status` and
`tag.<key>` are refused with `400 unknown_parameter`; to search audited calls by those, use
`/reports/requests?audited=true`, whose events carry the `audit_id`.

`GET /reports/audit?from=2026-03-12T00:00:00Z&to=2026-03-13T00:00:00Z&limit=1`

```json
{
  "from": "2026-03-12T00:00:00Z",
  "to": "2026-03-13T00:00:00Z",
  "limit": 1,
  "records": [
    {
      "audit_id": "aud-2",
      "event_id": "00000000-0000-0000-0000-000000000002",
      "timestamp": "2026-03-12T10:20:00Z",
      "expires_at": "2099-01-01T00:00:00Z",
      "organisation_id": "org-a",
      "workspace_id": "ws-1",
      "key_id": "key-1",
      "end_user": "alice",
      "session_id": "sess-1",
      "trace_id": "trace-1",
      "agent_id": "planner-bot",
      "user_id": "u-1",
      "task": "plan",
      "complexity": "high",
      "template": "planner",
      "model": "claude-haiku",
      "mode": "managed",
      "truncated": false
    }
  ],
  "next_cursor": "1773310800000000000:aud-2"
}
```

### `DELETE /audit`

Erases the audit records of one end user or one session. Note the path: it is not under
`/reports`.

| Parameter | |
|---|---|
| `end_user` | Erase the records of this end user |
| `session_id` | Erase the records of this session |
| `organisation_id`, `workspace_id` | Restrict the erasure to this scope |

At least one of `end_user` and `session_id` is required and must not be empty
(`400 missing_parameter` or `400 invalid_parameter`). When both are given, a record must match
both. Values are matched exactly. No other parameter is accepted.

The answer is sent after ClickHouse has rewritten the data: the records are removed from disk,
not hidden. Usage events are not touched; their `audit_id` then points to a record that no
longer exists.

`DELETE /audit?end_user=alice&organisation_id=org-a`

```json
{"deleted":2}
```

`deleted` is the number of records that matched. Erasing again returns `{"deleted":0}`.

## Erasure

### `DELETE /erasure`

Erases what the service holds about one user of one workspace (identity concept 6.9), by the
`user_id` or the `end_user` their calls carried: the usage events, the audit records (with the
attachments no other record of the workspace refers to) and, by `end_user`, the daily end-user
rollups. The hourly rollups carry neither and stay.

| Parameter | |
|---|---|
| `organisation_id`, `workspace_id` | The workspace; both required |
| `user_id` | Erase this user's data |
| `end_user` | Erase this end user's data |

Exactly one of `user_id` and `end_user` is required (`400 missing_parameter`), and no value
may be empty (`400 invalid_parameter`). Values are matched exactly. As with `DELETE /audit`, the
answer is sent after ClickHouse has rewritten the data.

`DELETE /erasure?organisation_id=org-a&workspace_id=ws-1&end_user=alice`

```json
{"usage_events":4,"audit_records":0,"end_user_rollups":0}
```

Each number is how many rows matched. Erasing again returns zeros.

## Errors

Every error has the same shape:

`GET /reports/usage?bucket=minute`

```json
{"error":{"code":"invalid_parameter","message":"bucket: expected hour, day, week or month"}}
```

`error.line` is added for `malformed_line`.

| Status | `error.code` | Meaning |
|---|---|---|
| `400` | `unknown_parameter` | A query parameter the endpoint does not have |
| `400` | `invalid_parameter` | A parameter with a value that is not allowed, or given twice where one value is expected; `from` not earlier than `to` |
| `400` | `missing_parameter` | `DELETE /audit` without `end_user` and `session_id`; `DELETE /erasure` without the workspace, or without exactly one of `user_id` and `end_user` |
| `400` | `invalid_id` | `/reports/requests/{id}` with an id that is not a UUID |
| `400` | `invalid_path` | A path segment that is not valid percent-encoded UTF-8 |
| `400` | `malformed_line`, `invalid_batch_id`, `unreadable_body` | Ingest, see above |
| `401` | `unauthorized` | Missing or wrong `x-gw-internal-token` |
| `404` | `not_found` | No such request, trace, audit record, or route |
| `405` | (no body) | The path exists, the method does not |
| `413` | `batch_too_large` | Ingest, see above |
| `415` | `unsupported_encoding` | Ingest, see above |
| `500` | `storage_error` | ClickHouse rejected a report query, or returned something unexpected. A defect; details are in the service log. |
| `500` | `encoding_failed` | A response could not be written. A defect. |
| `503` | `storage_unavailable` | ClickHouse is unreachable, or did not accept an insert |
| `503` | `not_ready` | The schema migrations have not been applied yet |

A report query that runs longer than 60 seconds is stopped and answered with
`500 storage_error`.

## Configuration

| Variable | Default | |
|---|---|---|
| `ANALYTICS_LISTEN` | `0.0.0.0:8090` | |
| `CLICKHOUSE_URL` | `http://localhost:8123` | ClickHouse's HTTP interface |
| `CLICKHOUSE_DATABASE` | `pacod_gw` | Created when missing. Letters, digits and underscores only. |
| `CLICKHOUSE_USER`, `CLICKHOUSE_PASSWORD` | unset | Unset: ClickHouse's default user |
| `PGW_INTERNAL_TOKEN` | required | At least 16 characters |
| `ANALYTICS_MAX_BATCH_BYTES` | `67108864` | Largest ingest body |
| `PGW_CORE_ADMIN_URL` | unset | The core's management API, for example `http://core:8081`. Set: this instance asks the core for batch jobs (the price tracker) every 30 seconds. Unset: it only ingests and reports. |
| `PGW_EGRESS_ALLOW_PRIVATE` | unset | Hosts, addresses and ranges that are not public and may still be fetched by a job, separated by commas. The same list the core reads. Without it a job fetches public addresses only. |
| `ANALYTICS_LOG_FORMAT` | text | `json` for one JSON object per line |
| `RUST_LOG` | `info` | Log level |

The ClickHouse user needs `CREATE DATABASE, CREATE TABLE, SELECT, INSERT, ALTER DELETE` on the
database.

The migrations in `migrations/*.sql` are compiled into the binary and applied at start-up; the
table `schema_migrations` records them. ClickHouse must not run those files itself (for example
from `/docker-entrypoint-initdb.d`).
