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
	// routing
	mode: string;
	interface: string;
	provider: string;
	model_requested: string;
	model_served: string;
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
