/**
 * The usage event: one record per call through the gateway.
 *
 * The OpenAPI document leaves the answers of `/requests` open, because the core forwards
 * them unchanged from the analytics service. This mirrors `UsageEvent` in
 * `crates/pacod-gw-events/src/usage.rs`, which serialises timing and usage flat into the
 * event, and the page shape documented in `crates/pacod-gw-analytics/API.md`.
 */
export interface UsageEvent {
	// identity
	id: string;
	timestamp: string;
	instance_id: string;
	// scope
	organisation_id: string;
	workspace_id: string;
	key_id: string;
	// attribution
	end_user: string;
	session_id: string;
	trace_id: string;
	tags: Record<string, string>;
	agent_id?: string;
	user_id?: string;
	task?: string;
	complexity?: string;
	// routing
	mode: string;
	interface: string;
	provider: string;
	model_requested: string;
	model_served: string;
	/** The model the provider says answered, in its own words. */
	model_reported?: string;
	template: string;
	template_version: number;
	path: string;
	// outcome
	status: string;
	http_status: number;
	error_type: string;
	error_message: string;
	finish_reason: string;
	attempts: number;
	stream: boolean;
	upstream_request_id: string;
	// timing, in milliseconds
	gateway_delay_ms: number;
	receive_ms: number;
	pre_ms: number;
	connect_ms: number;
	retry_ms: number;
	upstream_first_byte_ms: number;
	upstream_stream_ms: number;
	post_ms: number;
	relay_max_ms: number;
	settle_ms: number;
	total_ms: number;
	// usage
	input_tokens: number;
	cache_read_tokens: number;
	cache_write_tokens: number;
	cache_write_5m_tokens: number;
	cache_write_1h_tokens: number;
	output_tokens: number;
	reasoning_tokens: number;
	characters: number;
	server_tool_calls: number;
	usage_source: string;
	// cost
	cost: number | null;
	currency: string;
	price_version: number;
	price_variant: string;
	price_missing: boolean;
	// other
	dropped_params: string[];
	audit_id: string;
	/** Routing of a template with several models; empty for a single model. */
	routing_scheme?: string;
	routing_scheme_version?: number;
	routing_source?: string;
	routing_rule?: string;
	failovers?: number;
	models_tried?: string[];
	/** What plugins found and did (docs/plugin-concept.md); empty without plugins. */
	intent?: string;
	intent_confidence?: number;
	intent_engine?: string;
	plugin_verdict?: string;
	plugin_error?: string;
	/** The plugins' time before the provider call, apart from the gateway delay. */
	plugin_ms?: number;
	/** On a plugin's own call: the request id (`gw_…`) of the call it looked at. */
	parent_id?: string;
	/** What the response cache did: `hit`, `miss`, `would_hit`, `refresh` or `bypass`. */
	cache?: string;
	/** On a hit: what the kept answer would have cost, in `currency`. */
	cache_saved_cost?: number | null;
	/** The same in `reporting_currency`; unset without an exchange rate. */
	cache_saved_cost_reporting?: number | null;
}

/** The event id of a call from its request id: `gw_` and the id's 32 hex digits. */
export function eventIdOf(requestId: string): string | null {
	const hex = requestId.replace(/^gw_/, '');
	if (!/^[0-9a-f]{32}$/.test(hex)) return null;
	return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** One page of the request log, as `GET /requests` answers. */
export interface RequestPage {
	/** The time range the report used. */
	from: string | null;
	to: string | null;
	requests: UsageEvent[];
	nextCursor: string | null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Whether a value looks like a usage event: enough to render it without failing. */
export function isUsageEvent(value: unknown): value is UsageEvent {
	return isRecord(value) && typeof value.id === 'string' && typeof value.timestamp === 'string';
}

/** Reads the answer of `GET /requests`. Anything that is not an event is left out. */
export function parseRequestPage(body: unknown): RequestPage {
	const page = isRecord(body) ? body : {};
	const list = Array.isArray(page.requests) ? page.requests : [];
	return {
		from: typeof page.from === 'string' ? page.from : null,
		to: typeof page.to === 'string' ? page.to : null,
		requests: list.filter(isUsageEvent),
		nextCursor: typeof page.next_cursor === 'string' ? page.next_cursor : null
	};
}

/** The statuses a call can end with. */
export const STATUSES = ['ok', 'error', 'timeout', 'cancelled', 'rejected'] as const;
