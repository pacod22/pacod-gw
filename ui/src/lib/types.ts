// Mirrors crates/pacod-gw-events (UsageEvent). Keep in sync with the Rust schema.
export interface UsageEvent {
	id: string;
	timestamp: string;
	tenant_id: string;
	api_key_id: string;
	provider: string;
	model: string;
	route: string;
	status_code: number;
	latency_ms: number;
	prompt_tokens: number;
	completion_tokens: number;
	cost_usd: number;
	cached: boolean;
}
