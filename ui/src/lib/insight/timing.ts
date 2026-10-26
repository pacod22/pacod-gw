/**
 * The timing stages of one call, prepared for the stage bar.
 *
 * `gateway_delay_ms` is `pre_ms + post_ms`: the time by which the gateway delayed the answer.
 * The other stages lengthen a call too, but they belong to the client, the network or the
 * provider, so the bar colours a stage by who it belongs to.
 */

/** Who a stage's time belongs to. */
export type StageOwner = 'client' | 'gateway' | 'connection' | 'provider';

export interface StageDefinition {
	key:
		| 'receive_ms'
		| 'pre_ms'
		| 'connect_ms'
		| 'retry_ms'
		| 'upstream_first_byte_ms'
		| 'upstream_stream_ms'
		| 'post_ms';
	label: string;
	owner: StageOwner;
	help: string;
}

/** The stages in the order they happen. */
export const STAGES: readonly StageDefinition[] = [
	{
		key: 'receive_ms',
		label: 'Receive the request',
		owner: 'client',
		help: 'Reading the request from the client.'
	},
	{
		key: 'pre_ms',
		label: 'Gateway, before the provider call',
		owner: 'gateway',
		help: 'From the complete request to the start of the provider call: authentication, template, limits, translation.'
	},
	{
		key: 'connect_ms',
		label: 'Connect to the provider',
		owner: 'connection',
		help: 'Waiting for a pooled connection, or opening a new one. Zero when a connection is reused.'
	},
	{
		key: 'retry_ms',
		label: 'Retries',
		owner: 'connection',
		help: 'Failed attempts and the waits between them.'
	},
	{
		key: 'upstream_first_byte_ms',
		label: 'Provider, until its first byte',
		owner: 'provider',
		help: 'From the request sent to the provider to the first byte of its answer.'
	},
	{
		key: 'upstream_stream_ms',
		label: 'Provider, first byte to last',
		owner: 'provider',
		help: 'While the provider sends its answer.'
	},
	{
		key: 'post_ms',
		label: 'Gateway, on the answer',
		owner: 'gateway',
		help: 'For a stream: from the provider’s first byte to the first byte sent to the client. Otherwise: from the provider’s last byte to the last byte sent.'
	}
];

export const OWNERS: Record<StageOwner, string> = {
	client: 'Client',
	gateway: 'Gateway',
	connection: 'Connection and retries',
	provider: 'Provider'
};

export interface Stage extends StageDefinition {
	ms: number;
	/** The stage's part of the sum of all stages, 0 to 1. */
	share: number;
}

export interface Timing {
	stages: Stage[];
	/** The sum of the stages. In a stream stages overlap, so it can exceed `total`. */
	sum: number;
	/** From the first request byte to the last response byte. */
	total: number;
	/** `pre_ms + post_ms`. */
	gatewayDelay: number;
	/** The gateway delay as a part of the total, 0 to 1; `null` without a total. */
	gatewayShare: number | null;
}

function ms(value: unknown): number {
	return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : 0;
}

/** Splits an event's timing into the stages of the bar. Missing values count as zero. */
export function timingOf(
	event: Partial<Record<StageDefinition['key'] | 'total_ms' | 'gateway_delay_ms', unknown>>
): Timing {
	const values = STAGES.map((stage) => ({ ...stage, ms: ms(event[stage.key]) }));
	const sum = values.reduce((acc, stage) => acc + stage.ms, 0);
	const total = ms(event.total_ms);
	// The event carries the delay; older or partial records are answered from the stages.
	const gatewayDelay =
		typeof event.gateway_delay_ms === 'number' && Number.isFinite(event.gateway_delay_ms)
			? ms(event.gateway_delay_ms)
			: ms(event.pre_ms) + ms(event.post_ms);
	return {
		stages: values.map((stage) => ({ ...stage, share: sum > 0 ? stage.ms / sum : 0 })),
		sum,
		total,
		gatewayDelay,
		gatewayShare: total > 0 ? Math.min(1, gatewayDelay / total) : null
	};
}
