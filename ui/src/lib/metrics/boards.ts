/**
 * The Metrics page: the four Grafana dashboards of deploy/grafana/dashboards, panel by panel,
 * with the same queries. Placeholders: `$organisation`, `$workspace`, `$provider`, `$model` and
 * `$instance` are the filters (a regular expression, `.*` for all), `$rate` the window of a
 * rate and `$range` the whole range shown.
 */

export type Unit = 'reqps' | 'percent' | 'seconds' | 'short' | 'money' | 'bytes' | 'days' | 'count';

export interface Query {
	expr: string;
	/** The name of a series: `{{provider}} / {{model}}`, or a fixed name. Default: the labels. */
	legend?: string;
	/** In a table: the column the value goes in. */
	column?: string;
}

export interface Panel {
	kind: 'stat' | 'line' | 'table';
	title: string;
	description: string;
	unit: Unit;
	queries: Query[];
	/** Series on top of each other. */
	stacked?: boolean;
	/** A table's label columns, in order. */
	labels?: string[];
}

export interface Section {
	title?: string;
	panels: Panel[];
}

export type Filter = 'organisation' | 'workspace' | 'provider' | 'model' | 'instance';

export interface Board {
	id: string;
	title: string;
	description: string;
	filters: Filter[];
	sections: Section[];
}

const OWP = 'organisation=~"$organisation", workspace=~"$workspace", provider=~"$provider"';
const OW = 'organisation=~"$organisation", workspace=~"$workspace"';
const PM = 'provider=~"$provider", model=~"$model"';
const I = 'instance=~"$instance"';

const failedShare = (by: string, match: string) =>
	`sum ${by}(rate(gw_requests_total{${match}, outcome!~"ok|cancelled"}[$rate])) / sum ${by}(rate(gw_requests_total{${match}, outcome!="cancelled"}[$rate]))`;
const quantile = (q: number, metric: string, by: string, match: string, window = '$rate') =>
	`histogram_quantile(${q}, sum by (le${by ? `, ${by}` : ''}) (rate(${metric}_bucket{${match}}[${window}])))`;

const overview: Board = {
	id: 'overview',
	title: 'Overview',
	description: 'Traffic, failures, latency, tokens and spend of the whole gateway.',
	filters: ['organisation', 'workspace', 'provider'],
	sections: [
		{
			panels: [
				{
					kind: 'stat',
					title: 'Requests per second',
					description: 'Logical calls per second, all outcomes.',
					unit: 'reqps',
					queries: [{ expr: `sum(rate(gw_requests_total{${OWP}}[$rate]))` }]
				},
				{
					kind: 'stat',
					title: 'Failed calls',
					description:
						'Share of calls that ended in an error or a timeout. Calls the client cancelled are left out.',
					unit: 'percent',
					queries: [{ expr: failedShare('', OWP) }]
				},
				{
					kind: 'stat',
					title: 'Spend in the range',
					description: 'Cost of the calls settled in the range, per currency.',
					unit: 'money',
					queries: [
						{
							expr: `sum by (currency) (increase(gw_cost_micros_total{${OWP}}[$range])) / 1e6`,
							legend: '{{currency}}'
						}
					]
				},
				{
					kind: 'stat',
					title: 'Tokens in the range',
					description: 'Input, output, cache and reasoning tokens together.',
					unit: 'short',
					queries: [{ expr: `sum(increase(gw_tokens_total{${OWP}}[$range]))` }]
				},
				{
					kind: 'stat',
					title: 'Streams in flight',
					description: 'Calls that are being answered right now, on all instances.',
					unit: 'count',
					queries: [{ expr: 'sum(gw_streams_in_flight)' }]
				},
				{
					kind: 'stat',
					title: 'Gateway delay, 99th percentile',
					description: 'Time the gateway itself adds to a call, for the slower of the two modes.',
					unit: 'seconds',
					queries: [
						{
							expr: `max(${quantile(0.99, 'gw_delay_seconds', 'mode', 'provider=~"$provider", stage="total"', '5m')})`
						}
					]
				}
			]
		},
		{
			title: 'Traffic',
			panels: [
				{
					kind: 'line',
					title: 'Requests by outcome',
					description: 'Calls per second, stacked by how they ended.',
					unit: 'reqps',
					stacked: true,
					queries: [
						{
							expr: `sum by (outcome) (rate(gw_requests_total{${OWP}}[$rate]))`,
							legend: '{{outcome}}'
						}
					]
				},
				{
					kind: 'line',
					title: 'Requests by provider',
					description: 'Calls per second for each provider.',
					unit: 'reqps',
					stacked: true,
					queries: [
						{
							expr: `sum by (provider) (rate(gw_requests_total{${OWP}}[$rate]))`,
							legend: '{{provider}}'
						}
					]
				}
			]
		},
		{
			title: 'Failures and latency',
			panels: [
				{
					kind: 'line',
					title: 'Failed calls by provider',
					description: 'Share of calls that failed, per provider.',
					unit: 'percent',
					queries: [{ expr: failedShare('by (provider) ', OWP), legend: '{{provider}}' }]
				},
				{
					kind: 'line',
					title: 'Requests by mode',
					description: 'Calls per second in proxy mode and in managed mode.',
					unit: 'reqps',
					stacked: true,
					queries: [
						{ expr: `sum by (mode) (rate(gw_requests_total{${OWP}}[$rate]))`, legend: '{{mode}}' }
					]
				},
				{
					kind: 'line',
					title: 'Request duration',
					description:
						'From the first request byte to the last response byte, provider time included.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.5, 'gw_request_duration_seconds', '', 'provider=~"$provider"'),
							legend: 'median'
						},
						{
							expr: quantile(0.95, 'gw_request_duration_seconds', '', 'provider=~"$provider"'),
							legend: '95th percentile'
						},
						{
							expr: quantile(0.99, 'gw_request_duration_seconds', '', 'provider=~"$provider"'),
							legend: '99th percentile'
						}
					]
				},
				{
					kind: 'line',
					title: 'Time to first byte',
					description: 'From the request sent to the provider to the first byte of its answer.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.5, 'gw_first_byte_seconds', '', 'provider=~"$provider"'),
							legend: 'median'
						},
						{
							expr: quantile(0.95, 'gw_first_byte_seconds', '', 'provider=~"$provider"'),
							legend: '95th percentile'
						}
					]
				}
			]
		},
		{
			title: 'Usage and cost',
			panels: [
				{
					kind: 'line',
					title: 'Spend per hour by provider',
					description: 'Cost of settled calls, as an hourly rate.',
					unit: 'money',
					stacked: true,
					queries: [
						{
							expr: `sum by (provider, currency) (rate(gw_cost_micros_total{${OWP}}[$rate])) * 3600 / 1e6`,
							legend: '{{provider}} ({{currency}})'
						}
					]
				},
				{
					kind: 'line',
					title: 'Tokens per second by kind',
					description:
						'Token throughput by kind: input, output, cache read, cache write, reasoning.',
					unit: 'short',
					stacked: true,
					queries: [
						{ expr: `sum by (kind) (rate(gw_tokens_total{${OWP}}[$rate]))`, legend: '{{kind}}' }
					]
				}
			]
		}
	]
};

const PROVIDER_MODEL = '{{provider}} / {{model}}';

const providers: Board = {
	id: 'providers',
	title: 'Providers and models',
	description: 'Volume, failures, latency and cost per provider and model.',
	filters: ['provider', 'model'],
	sections: [
		{
			title: 'Volume',
			panels: [
				{
					kind: 'line',
					title: 'Requests by model',
					description: 'Calls per second for the ten busiest models.',
					unit: 'reqps',
					queries: [
						{
							expr: `topk(10, sum by (provider, model) (rate(gw_requests_total{${PM}}[$rate])))`,
							legend: PROVIDER_MODEL
						}
					]
				},
				{
					kind: 'line',
					title: 'Tokens by model',
					description: 'Tokens per second for the ten busiest models.',
					unit: 'short',
					queries: [
						{
							expr: `topk(10, sum by (provider, model) (rate(gw_tokens_total{${PM}}[$rate])))`,
							legend: PROVIDER_MODEL
						}
					]
				}
			]
		},
		{
			title: 'Failures',
			panels: [
				{
					kind: 'line',
					title: 'Failed calls by model',
					description: 'Share of calls that failed, per provider and model.',
					unit: 'percent',
					queries: [{ expr: failedShare('by (provider, model) ', PM), legend: PROVIDER_MODEL }]
				},
				{
					kind: 'line',
					title: 'Timeouts per minute',
					description: 'Calls that ran into the time limit. A timeout is never retried.',
					unit: 'short',
					queries: [
						{
							expr: `sum by (provider, model) (rate(gw_requests_total{${PM}, outcome="timeout"}[$rate])) * 60`,
							legend: PROVIDER_MODEL
						}
					]
				},
				{
					kind: 'line',
					title: 'Retries per minute',
					description:
						'Attempts repeated before the first byte, after a connection error, a 429 or a 5xx.',
					unit: 'short',
					queries: [
						{
							expr: `sum by (provider, model) (rate(gw_retries_total{${PM}}[$rate])) * 60`,
							legend: PROVIDER_MODEL
						}
					]
				},
				{
					kind: 'line',
					title: 'Requests by outcome',
					description: 'Calls per second for the selected providers and models, by how they ended.',
					unit: 'reqps',
					stacked: true,
					queries: [
						{
							expr: `sum by (outcome) (rate(gw_requests_total{${PM}}[$rate]))`,
							legend: '{{outcome}}'
						}
					]
				}
			]
		},
		{
			title: 'Latency',
			panels: [
				{
					kind: 'line',
					title: 'Request duration by provider, 95th percentile',
					description: 'From the first request byte to the last response byte.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(
								0.95,
								'gw_request_duration_seconds',
								'provider',
								'provider=~"$provider"'
							),
							legend: '{{provider}}'
						}
					]
				},
				{
					kind: 'line',
					title: 'Time to first byte by provider, 95th percentile',
					description: 'From the request sent to the provider to the first byte of its answer.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.95, 'gw_first_byte_seconds', 'provider', 'provider=~"$provider"'),
							legend: '{{provider}}'
						}
					]
				}
			]
		},
		{
			title: 'Cost',
			panels: [
				{
					kind: 'line',
					title: 'Spend per hour by model',
					description:
						'Cost of settled calls as an hourly rate, for the ten models that cost most.',
					unit: 'money',
					queries: [
						{
							expr: `topk(10, sum by (provider, model, currency) (rate(gw_cost_micros_total{${PM}}[$rate])) * 3600 / 1e6)`,
							legend: '{{provider}} / {{model}} ({{currency}})'
						}
					]
				},
				{
					kind: 'table',
					title: 'Spend in the range',
					description: 'Cost of settled calls per provider and model.',
					unit: 'money',
					labels: ['provider', 'model', 'currency'],
					queries: [
						{
							expr: `sort_desc(sum by (provider, model, currency) (increase(gw_cost_micros_total{${PM}}[$range])) / 1e6)`,
							column: 'cost'
						}
					]
				},
				{
					kind: 'line',
					title: 'Calls settled without a price',
					description:
						'Calls per minute that could not be priced because the model has no price in effect.',
					unit: 'short',
					queries: [
						{
							expr: `sum by (provider, model) (rate(gw_price_missing_total{${PM}}[$rate])) * 60`,
							legend: PROVIDER_MODEL
						}
					]
				},
				{
					kind: 'line',
					title: 'Share of recent calls that succeeded',
					description:
						'For each provider and model with calls in the last minutes, the share that succeeded.',
					unit: 'percent',
					queries: [
						{
							expr: `min by (provider, model) (gw_upstream_health{${PM}})`,
							legend: PROVIDER_MODEL
						}
					]
				},
				{
					kind: 'line',
					title: 'Days since each price was checked',
					description: 'Days since the price in effect for a model was entered or last confirmed.',
					unit: 'days',
					queries: [
						{ expr: 'max by (model) (gw_price_age_days{model=~"$model"})', legend: '{{model}}' }
					]
				}
			]
		}
	]
};

const ORG_WS = '{{organisation}} / {{workspace}}';

const workspaces: Board = {
	id: 'workspaces',
	title: 'Workspaces',
	description: 'Traffic, spend and limits per organisation and workspace.',
	filters: ['organisation', 'workspace'],
	sections: [
		{
			panels: [
				{
					kind: 'stat',
					title: 'Active workspaces',
					description: 'Workspaces with at least one call in the last five minutes.',
					unit: 'count',
					queries: [
						{
							expr: `count(sum by (organisation, workspace) (rate(gw_requests_total{${OW}}[5m])) > 0)`
						}
					]
				},
				{
					kind: 'stat',
					title: 'Requests per second',
					description: 'Calls per second in the selected workspaces.',
					unit: 'reqps',
					queries: [{ expr: `sum(rate(gw_requests_total{${OW}}[$rate]))` }]
				},
				{
					kind: 'stat',
					title: 'Spend in the range',
					description: 'Cost of settled calls in the selected workspaces, per currency.',
					unit: 'money',
					queries: [
						{
							expr: `sum by (currency) (increase(gw_cost_micros_total{${OW}}[$range])) / 1e6`,
							legend: '{{currency}}'
						}
					]
				},
				{
					kind: 'stat',
					title: 'Tokens in the range',
					description: 'All token kinds together.',
					unit: 'short',
					queries: [{ expr: `sum(increase(gw_tokens_total{${OW}}[$range]))` }]
				}
			]
		},
		{
			title: 'Traffic',
			panels: [
				{
					kind: 'line',
					title: 'Requests by workspace',
					description: 'Calls per second for the ten busiest workspaces.',
					unit: 'reqps',
					queries: [
						{
							expr: `topk(10, sum by (organisation, workspace) (rate(gw_requests_total{${OW}}[$rate])))`,
							legend: ORG_WS
						}
					]
				},
				{
					kind: 'line',
					title: 'Failed calls by workspace',
					description: 'Share of calls that failed, per workspace.',
					unit: 'percent',
					queries: [{ expr: failedShare('by (organisation, workspace) ', OW), legend: ORG_WS }]
				}
			]
		},
		{
			title: 'Usage and cost',
			panels: [
				{
					kind: 'line',
					title: 'Spend per hour by workspace',
					description:
						'Cost of settled calls as an hourly rate, for the ten workspaces that spend most.',
					unit: 'money',
					queries: [
						{
							expr: `topk(10, sum by (organisation, workspace, currency) (rate(gw_cost_micros_total{${OW}}[$rate])) * 3600 / 1e6)`,
							legend: '{{organisation}} / {{workspace}} ({{currency}})'
						}
					]
				},
				{
					kind: 'line',
					title: 'Tokens per second by kind',
					description: 'Token throughput of the selected workspaces by kind.',
					unit: 'short',
					stacked: true,
					queries: [
						{ expr: `sum by (kind) (rate(gw_tokens_total{${OW}}[$rate]))`, legend: '{{kind}}' }
					]
				},
				{
					kind: 'table',
					title: 'Spend in the range',
					description: 'Cost of settled calls per workspace and model.',
					unit: 'money',
					labels: ['organisation', 'workspace', 'model', 'currency'],
					queries: [
						{
							expr: `sort_desc(sum by (organisation, workspace, model, currency) (increase(gw_cost_micros_total{${OW}}[$range])) / 1e6)`,
							column: 'cost'
						}
					]
				},
				{
					kind: 'table',
					title: 'Requests and tokens in the range',
					description: 'Calls and tokens per workspace.',
					unit: 'short',
					labels: ['organisation', 'workspace'],
					queries: [
						{
							expr: `sum by (organisation, workspace) (increase(gw_requests_total{${OW}}[$range]))`,
							column: 'requests'
						},
						{
							expr: `sum by (organisation, workspace) (increase(gw_tokens_total{${OW}}[$range]))`,
							column: 'tokens'
						}
					]
				}
			]
		},
		{
			title: 'Limits',
			panels: [
				{
					kind: 'line',
					title: 'Share of each limit that is used',
					description:
						'For every limit that counts a whole workspace or a whole organisation: the used share of the allowance.',
					unit: 'percent',
					queries: [
						{
							expr: 'max by (organisation, workspace, limit) (gw_limit_used_ratio{organisation=~"$organisation", workspace=~"$workspace|"})',
							legend: '{{organisation}} / {{workspace}} / {{limit}}'
						}
					]
				}
			]
		}
	]
};

const internals: Board = {
	id: 'internals',
	title: 'Gateway internals',
	description: 'Gateway delay, back-pressure, spools and the configuration rollout per instance.',
	filters: ['instance'],
	sections: [
		{
			panels: [
				{
					kind: 'stat',
					title: 'Delay in proxy mode, median',
					description: 'Median of the time the gateway adds to a proxied call. Target: 0.5 ms.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.5, 'gw_delay_seconds', '', `${I}, mode="proxy", stage="total"`, '5m')
						}
					]
				},
				{
					kind: 'stat',
					title: 'Delay in proxy mode, 99th percentile',
					description:
						'Target: 2 ms. The alert fires when this stays above the target for 10 minutes.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(
								0.99,
								'gw_delay_seconds',
								'',
								`${I}, mode="proxy", stage="total"`,
								'5m'
							)
						}
					]
				},
				{
					kind: 'stat',
					title: 'Delay in managed mode, 99th percentile',
					description:
						'Target: 5 ms in the same dialect, 8 ms with translation. The alert fires above 8 ms.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(
								0.99,
								'gw_delay_seconds',
								'',
								`${I}, mode="managed", stage="total"`,
								'5m'
							)
						}
					]
				},
				{
					kind: 'stat',
					title: 'Streams in flight',
					description: 'Calls being answered right now on the selected instances.',
					unit: 'count',
					queries: [{ expr: `sum(gw_streams_in_flight{${I}})` }]
				},
				{
					kind: 'stat',
					title: 'Events dropped in the range',
					description:
						'Usage events lost because the spool was full. Above zero, reports are incomplete.',
					unit: 'count',
					queries: [{ expr: `sum(increase(gw_events_dropped_total{${I}}[$range]))` }]
				},
				{
					kind: 'stat',
					title: 'Audit records dropped in the range',
					description: 'Audit-mode records lost because the audit spool was full.',
					unit: 'count',
					queries: [{ expr: `sum(increase(gw_audit_records_dropped_total{${I}}[$range]))` }]
				}
			]
		},
		{
			title: 'Gateway delay',
			panels: [
				{
					kind: 'line',
					title: 'Delay in proxy mode by stage, 99th percentile',
					description:
						'Time added by the gateway: "total" is the gateway delay, the others its parts.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.99, 'gw_delay_seconds', 'stage', `${I}, mode="proxy"`),
							legend: '{{stage}}'
						}
					]
				},
				{
					kind: 'line',
					title: 'Delay in managed mode by stage, 99th percentile',
					description:
						'Time added by the gateway per stage. The target is 8 ms for calls with translation.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.99, 'gw_delay_seconds', 'stage', `${I}, mode="managed"`),
							legend: '{{stage}}'
						}
					]
				},
				{
					kind: 'line',
					title: 'Delay by provider, 99th percentile',
					description: 'The gateway delay per provider. One that stands out points at its adapter.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.99, 'gw_delay_seconds', 'provider', `${I}, stage="total"`),
							legend: '{{provider}}'
						}
					]
				},
				{
					kind: 'line',
					title: 'Delay by mode, median',
					description:
						'The typical gateway delay per mode. Targets: 0.5 ms in proxy mode, 1 ms in managed mode.',
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.5, 'gw_delay_seconds', 'mode', `${I}, stage="total"`),
							legend: '{{mode}}'
						}
					]
				}
			]
		},
		{
			title: 'Load and back-pressure',
			panels: [
				{
					kind: 'line',
					title: 'Streams in flight by instance',
					description: 'Calls being answered. Above the per-instance cap the gateway answers 503.',
					unit: 'count',
					stacked: true,
					queries: [
						{ expr: `sum by (instance) (gw_streams_in_flight{${I}})`, legend: '{{instance}}' }
					]
				},
				{
					kind: 'line',
					title: 'Rejections by reason',
					description: 'Requests the gateway refused, per second.',
					unit: 'reqps',
					queries: [
						{
							expr: `sum by (reason) (rate(gw_rejections_total{${I}}[$rate]))`,
							legend: '{{reason}}'
						}
					]
				}
			]
		},
		{
			title: 'Event and audit spools',
			panels: [
				{
					kind: 'line',
					title: 'Event spool size',
					description:
						'Usage events waiting to be shipped to the analytics service. It should stay near zero.',
					unit: 'bytes',
					queries: [{ expr: `max by (instance) (gw_spool_bytes{${I}})`, legend: '{{instance}}' }]
				},
				{
					kind: 'line',
					title: 'Audit spool size',
					description: 'Audit-mode records waiting to be shipped.',
					unit: 'bytes',
					queries: [
						{ expr: `max by (instance) (gw_audit_spool_bytes{${I}})`, legend: '{{instance}}' }
					]
				},
				{
					kind: 'line',
					title: 'Events dropped per minute',
					description: 'Usage events lost because the spool was full.',
					unit: 'short',
					queries: [
						{
							expr: `sum by (instance) (rate(gw_events_dropped_total{${I}}[$rate])) * 60`,
							legend: '{{instance}}'
						}
					]
				},
				{
					kind: 'line',
					title: 'Audit records dropped per minute',
					description: 'Audit-mode records lost because the audit spool was full.',
					unit: 'short',
					queries: [
						{
							expr: `sum by (instance) (rate(gw_audit_records_dropped_total{${I}}[$rate])) * 60`,
							legend: '{{instance}}'
						}
					]
				}
			]
		},
		{
			title: 'Configuration rollout',
			panels: [
				{
					kind: 'line',
					title: 'Configuration version by instance',
					description:
						'The configuration version each instance has applied. All lines should lie on top of each other.',
					unit: 'count',
					queries: [{ expr: `max by (instance) (gw_config_version{${I}})`, legend: '{{instance}}' }]
				},
				{
					kind: 'line',
					title: 'Configuration versions behind',
					description: 'How many configuration versions an instance has not applied yet.',
					unit: 'count',
					queries: [
						{
							expr: `max by (instance) (gw_config_apply_lag_versions{${I}})`,
							legend: '{{instance}}'
						}
					]
				}
			]
		},
		{
			title: 'Response cache',
			panels: [
				{
					kind: 'line',
					title: 'Response cache: what it did, per minute',
					description:
						"Calls answered from a kept answer (hit), looked for and not found (miss), found by a binding in shadow mode (would_hit), refreshed or bypassed at the caller's request or because they were not deterministic; and answers kept (stored) or not worth keeping (not_kept).",
					unit: 'short',
					queries: [
						{
							expr: `sum by (result) (rate(gw_response_cache_total{${I}}[$rate])) * 60`,
							legend: '{{result}}'
						}
					]
				},
				{
					kind: 'line',
					title: 'Response cache hits',
					description:
						'The share of the calls the cache looked up that it answered: hits over hits, misses and would-be hits.',
					unit: 'percent',
					queries: [
						{
							expr: `sum(rate(gw_response_cache_total{${I}, result="hit"}[$rate])) / sum(rate(gw_response_cache_total{${I}, result=~"hit|miss|would_hit"}[$rate]))`,
							legend: 'hits'
						}
					]
				},
				{
					kind: 'line',
					title: 'Response cache lookups, 95th percentile',
					description:
						"How long a call waits for the cache's store before it is answered from it or goes on to its provider. Bounded by the binding's timeout (100 ms by default).",
					unit: 'seconds',
					queries: [
						{
							expr: quantile(0.95, 'gw_response_cache_lookup_seconds', 'store', I),
							legend: '{{store}}'
						}
					]
				},
				{
					kind: 'line',
					title: 'Response cache store errors per minute',
					description:
						'The store failed to read (get) or to keep (put) an answer, or was skipped while it was failing (skipped). Calls are then served by their providers, without the cache.',
					unit: 'short',
					queries: [
						{
							expr: `sum by (store, op) (rate(gw_response_cache_errors_total{${I}}[$rate])) * 60`,
							legend: '{{store}} {{op}}'
						}
					]
				}
			]
		}
	]
};

export const BOARDS: Board[] = [overview, providers, workspaces, internals];

export function boardOf(id: string): Board | undefined {
	return BOARDS.find((b) => b.id === id);
}

/** The ranges to choose from, as in Grafana's quick ranges. */
export const METRIC_RANGES = [
	{ id: '1h', label: 'Last hour', seconds: 3600 },
	{ id: '6h', label: 'Last 6 hours', seconds: 6 * 3600 },
	{ id: '24h', label: 'Last 24 hours', seconds: 24 * 3600 },
	{ id: '7d', label: 'Last 7 days', seconds: 7 * 24 * 3600 }
];

export function metricRangeOf(id: string | null | undefined) {
	return METRIC_RANGES.find((r) => r.id === id) ?? METRIC_RANGES[1];
}

/** Prometheus scrapes every 15 seconds (deploy/prometheus/prometheus.yml). */
const SCRAPE_SECONDS = 15;

/**
 * The step between points (about 120 over the range, never below the scrape interval) and the
 * window of a rate, as Grafana's `$__rate_interval`: one step and one scrape, at least four
 * scrapes.
 */
export function stepsOf(seconds: number): { step: number; rate: number } {
	const step = Math.max(SCRAPE_SECONDS, Math.ceil(seconds / 120 / SCRAPE_SECONDS) * SCRAPE_SECONDS);
	return { step, rate: Math.max(step + SCRAPE_SECONDS, 4 * SCRAPE_SECONDS) };
}

/** A value as a regular expression that matches it alone, written for a PromQL string. */
function exactly(value: string): string {
	const regex = value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	return regex.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
}

/** A query with its placeholders filled in. A filter without a value matches everything. */
export function expand(
	expr: string,
	filters: Partial<Record<Filter, string>>,
	rate: number,
	range: number
): string {
	return expr
		.replace(/\$(organisation|workspace|provider|model|instance)\b/g, (_, name: Filter) =>
			filters[name] ? exactly(filters[name]) : '.*'
		)
		.replaceAll('$rate', `${rate}s`)
		.replaceAll('$range', `${range}s`);
}
