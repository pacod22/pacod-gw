import { describe, expect, it } from 'vitest';
import { timeline } from './timeline.ts';

describe('timeline', () => {
	it('puts model and tool calls in time order, a fraction after its whole second', () => {
		const rows = timeline(
			[
				{
					id: 'r1',
					timestamp: '2026-03-12T10:05:00Z',
					model_served: 'claude-haiku',
					status: 'ok',
					total_ms: 900
				},
				{ id: 'r2', timestamp: '2026-03-12T10:05:02Z', model_requested: 'gpt-x', status: 'error' }
			],
			[
				{
					timestamp: '2026-03-12T10:05:00.5Z',
					protocol: 'mcp',
					exposed_name: 'github__search_issues',
					verdict: 'allowed',
					status: 'ok',
					total_ms: 125.25
				},
				{
					timestamp: '2026-03-12T10:05:01Z',
					protocol: 'a2a',
					target: 'researcher',
					operation: 'SendMessage',
					verdict: 'denied',
					status: 'rejected'
				}
			]
		);
		expect(rows.map((r) => [r.kind, r.call, r.status])).toEqual([
			['Model', 'claude-haiku', 'ok'],
			['Tool', 'github__search_issues', 'ok'],
			['Agent', 'researcher · SendMessage', 'denied'],
			['Model', 'gpt-x', 'error']
		]);
		expect(rows.map((r) => r.request_id)).toEqual(['r1', null, null, 'r2']);
		expect(rows[1].total_ms).toBe(125.25);
		expect(rows[2].total_ms).toBeNull();
	});

	it('keeps calls of the same instant in the order they came, models first', () => {
		const at = '2026-03-12T10:05:00Z';
		const rows = timeline(
			[{ id: 'r1', timestamp: at, model_served: 'm' }],
			[{ timestamp: at, protocol: 'mcp', exposed_name: 't', verdict: 'allowed', status: 'ok' }]
		);
		expect(rows.map((r) => r.kind)).toEqual(['Model', 'Tool']);
	});
});
