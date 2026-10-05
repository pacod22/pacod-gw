import { describe, expect, it } from 'vitest';
import { formatValue, labelOf, rowsOf } from '../report/format';
import { chartPoints, seriesRows } from '../report/series';
import { bodyText, factsOf, lineDiff, partsOf, pretty, requestDiff } from './audit';

const part = (over: Record<string, unknown>) => ({
	method: '',
	url: '',
	status: 0,
	headers: [['content-type', 'application/json']],
	body: '',
	body_encoding: 'utf8',
	body_bytes: 0,
	same_as: '',
	chunks: [],
	truncated: false,
	...over
});

const record = {
	client_request: part({
		method: 'POST',
		url: '/v1/messages',
		body: '{"model":"@planner","max_tokens":8000,"messages":[]}'
	}),
	provider_requests: [
		part({
			method: 'POST',
			url: 'http://provider/v1/messages',
			body: '{"model":"claude","max_tokens":400,"messages":[]}'
		})
	],
	provider_responses: [part({ status: 200, body: '{"id":"msg_1"}', body_bytes: 14 })],
	client_response: part({
		status: 200,
		body_encoding: 'ref',
		body_bytes: 14,
		same_as: 'provider_responses[0]'
	})
};

describe('the parts of an audit record', () => {
	it('lists the four parts in the order of the call', () => {
		expect(partsOf(record).map((p) => p.name)).toEqual([
			'client_request',
			'provider_requests[0]',
			'provider_responses[0]',
			'client_response'
		]);
		expect(partsOf(null)).toEqual([]);
	});

	it('numbers the attempts when there were several', () => {
		const retried = {
			...record,
			provider_requests: [record.provider_requests[0], record.provider_requests[0]]
		};
		expect(partsOf(retried).map((p) => p.title)).toContain('Request to the provider, attempt 2');
	});

	it('takes a referenced body from the part it names', () => {
		const parts = partsOf(record);
		const response = bodyText(parts, parts[3]);
		expect(response.text).toBe(pretty('{"id":"msg_1"}'));
		expect(response.note).toMatch(/same 14 bytes as response of the provider/);
		const binary = partsOf({
			client_request: part({ body: 'AAEC', body_encoding: 'base64', body_bytes: 3 })
		});
		expect(bodyText(binary, binary[0]).note).toMatch(/Binary, 3 bytes/);
	});
});

describe('what the gateway changed', () => {
	it('is a line difference of the two request bodies', () => {
		const diff = requestDiff(partsOf(record)) ?? [];
		expect(diff.filter((l) => l.type === 'removed').map((l) => l.text.trim())).toEqual([
			'"model": "@planner",',
			'"max_tokens": 8000,'
		]);
		expect(diff.filter((l) => l.type === 'added').map((l) => l.text.trim())).toEqual([
			'"model": "claude",',
			'"max_tokens": 400,'
		]);
		expect(diff.some((l) => l.type === 'same' && l.text.includes('"messages"'))).toBe(true);
	});

	it('keeps the order of unchanged lines and reports pure additions and removals', () => {
		expect(lineDiff(['a', 'b', 'c'], ['a', 'c', 'd'])).toEqual([
			{ type: 'same', text: 'a' },
			{ type: 'removed', text: 'b' },
			{ type: 'same', text: 'c' },
			{ type: 'added', text: 'd' }
		]);
		expect(lineDiff([], ['x'])).toEqual([{ type: 'added', text: 'x' }]);
		expect(lineDiff(['x'], ['x']).every((l) => l.type === 'same')).toBe(true);
	});

	it('does not compare binary bodies', () => {
		expect(
			requestDiff(partsOf({ ...record, client_request: part({ body_encoding: 'base64' }) }))
		).toBeNull();
	});
});

describe('facts and report values', () => {
	it('writes objects as label and text', () => {
		expect(
			factsOf({
				route: 'same_dialect',
				dropped_params: [],
				warnings: ['a', 'b'],
				template: { name: 'planner' },
				end_user: ''
			})
		).toEqual([
			{ label: 'route', text: 'same_dialect' },
			{ label: 'dropped params', text: 'None' },
			{ label: 'warnings', text: 'a; b' },
			{ label: 'template', text: '{"name":"planner"}' },
			{ label: 'end user', text: '–' }
		]);
	});

	it('formats a report value by the name of its field', () => {
		expect(formatValue('requests', 12000)).toBe('12,000');
		expect(formatValue('error_rate', 0.0833)).toBe('8.3%');
		expect(formatValue('gateway_delay_p50_ms', 0.130958)).toBe('0.13 ms');
		expect(formatValue('last_seen_at', '2026-10-05T10:35:42.560710Z')).toBe('2026-10-05 10:35');
		expect(formatValue('group', '')).toBe('');
		expect(labelOf('gateway_delay_p99_ms')).toBe('Gateway delay, 99th percentile');
		expect(labelOf('input_tokens')).toBe('Input tokens');
		expect(rowsOf({ rows: [{ a: 1 }, 'x'] }, 'rows')).toEqual([{ a: 1 }]);
	});

	it('sums a measure per bucket over the groups of a series', () => {
		const usage = {
			series: [
				{
					group: 'a',
					points: [{ bucket: '2026-10-05T10:00:00Z', requests: 2, gateway_delay_p99_ms: 1 }]
				},
				{
					group: 'b',
					points: [
						{ bucket: '2026-10-05T10:00:00Z', requests: 3, gateway_delay_p99_ms: 4 },
						{ bucket: '2026-10-05T09:00:00Z', requests: 1, gateway_delay_p99_ms: 2 }
					]
				}
			]
		};
		expect(seriesRows(usage)).toHaveLength(3);
		expect(chartPoints(usage, 'requests', 'hour')).toEqual([
			{ label: '10-05 09:00', value: 1 },
			{ label: '10-05 10:00', value: 5 }
		]);
		expect(chartPoints(usage, 'gateway_delay_p99_ms', 'hour', 'max')[1].value).toBe(4);
	});
});

describe('tags and grouping in reports', () => {
	it('offers the declared tag keys, or a key to type', async () => {
		const { groupOptions, groupLabel } = await import('../report/format');
		expect(groupOptions(['model'], ['purpose', 'site']).map((o) => o.value)).toEqual([
			'model',
			'tag:purpose',
			'tag:site'
		]);
		expect(groupOptions(['model'], null).map((o) => o.value)).toEqual(['model', 'tag']);
		expect(groupLabel('tag:purpose')).toBe('tag purpose');
		expect(groupLabel('key_id')).toBe('key id');
	});

	it('keeps the filters when paging by offset', async () => {
		const { offsetLink } = await import('../nav');
		expect(
			offsetLink({ pathname: '/insight/end-users', search: '?workspace_id=ws_1&offset=50' }, 100)
		).toBe('/insight/end-users?workspace_id=ws_1&offset=100');
		expect(
			offsetLink({ pathname: '/insight/end-users', search: '?workspace_id=ws_1&offset=50' }, 0)
		).toBe('/insight/end-users?workspace_id=ws_1');
	});
});
