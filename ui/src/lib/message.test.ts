import { describe, expect, it } from 'vitest';
import {
	buildBody,
	compose,
	defaultAuthHeader,
	defaultDraft,
	gatewayBlock,
	gatewayHeaders,
	methodAndPath,
	newTraceparent,
	parseTags,
	restoreDraft,
	type Draft
} from './message';

const draft = (more: Partial<Draft> = {}): Draft => ({
	...defaultDraft('org_1', 'ws_1'),
	model: 'acme/model',
	system: 'Be brief.',
	messages: [
		{ role: 'user', content: 'Hi' },
		{ role: 'assistant', content: 'Hello' },
		{ role: 'user', content: '' }
	],
	maxTokens: '64',
	temperature: '0.2',
	stop: 'END\n\nSTOP',
	...more
});

describe('buildBody', () => {
	it('writes the OpenAI shape: system as a message, max_tokens, stop, stream options', () => {
		expect(buildBody(draft({ stream: true }))).toEqual({
			model: 'acme/model',
			messages: [
				{ role: 'system', content: 'Be brief.' },
				{ role: 'user', content: 'Hi' },
				{ role: 'assistant', content: 'Hello' }
			],
			max_tokens: 64,
			stop: ['END', 'STOP'],
			temperature: 0.2,
			stream: true,
			stream_options: { include_usage: true }
		});
	});

	it('writes the Anthropic shape: system on top, stop_sequences, max_tokens always', () => {
		const body = buildBody(draft({ interface: 'anthropic', maxTokens: '' }));
		expect(body).toMatchObject({
			model: 'acme/model',
			system: 'Be brief.',
			max_tokens: 1024,
			stop_sequences: ['END', 'STOP']
		});
		expect(body?.messages).toEqual([
			{ role: 'user', content: 'Hi' },
			{ role: 'assistant', content: 'Hello' }
		]);
		expect(body).not.toHaveProperty('stream_options');
	});

	it('writes the native shape: content parts, max_output_tokens, a gateway block', () => {
		const d = draft({ interface: 'native', gatewayIn: 'body', stream: true });
		d.gateway = { ...d.gateway, endUser: 'u1', tags: 'purpose=test', retries: '2', audit: true };
		expect(buildBody(d)).toEqual({
			model: 'acme/model',
			system: [{ type: 'text', text: 'Be brief.' }],
			messages: [
				{ role: 'user', content: [{ type: 'text', text: 'Hi' }] },
				{ role: 'assistant', content: [{ type: 'text', text: 'Hello' }] }
			],
			max_output_tokens: 64,
			stop: ['END', 'STOP'],
			temperature: 0.2,
			stream: true,
			gateway: {
				end_user: 'u1',
				tags: { purpose: 'test' },
				retry: { max_attempts: 2 },
				audit: true
			}
		});
	});

	it('writes a proxied body in the dialect of its path', () => {
		expect(buildBody(draft({ interface: 'proxy', proxyPath: '/v1/messages' }))).toHaveProperty(
			'stop_sequences'
		);
		expect(buildBody(draft({ interface: 'proxy' }))).toHaveProperty('stop');
		expect(buildBody(draft({ interface: 'models' }))).toBeNull();
	});
});

describe('compose', () => {
	it('puts the gateway options in headers, with Anthropic its version', () => {
		const d = draft({ interface: 'anthropic' });
		d.gateway = { ...d.gateway, endUser: 'u1', complexity: 'low', audit: true };
		const c = compose(d);
		expect(c.method).toBe('POST');
		expect(c.path).toBe('/v1/messages');
		expect(c.headers).toEqual({
			'anthropic-version': '2023-06-01',
			'x-gw-end-user': 'u1',
			'x-gw-complexity': 'low',
			'x-gw-audit': 'true'
		});
		expect(c.problems).toEqual([]);
	});

	it('keeps the audit request in a header when the options go into the body', () => {
		const c = compose(draft({ gatewayIn: 'body' }));
		expect(c.headers).toEqual({ 'x-gw-audit': 'true' });
		expect(JSON.parse(c.body ?? '{}').gateway).toEqual({ audit: true });
	});

	it('lets extra headers replace the form and refuses the key headers', () => {
		const c = compose(
			draft({
				interface: 'anthropic',
				extraHeaders: [
					{ name: 'Anthropic-Version', value: '2024-01-01' },
					{ name: 'Authorization', value: 'Bearer x' },
					{ name: 'x-bad', value: 'a\r\nb' }
				]
			})
		);
		expect(c.headers['Anthropic-Version']).toBe('2024-01-01');
		expect(c.headers['anthropic-version']).toBeUndefined();
		expect(c.problems).toHaveLength(2);
	});

	it('merges additional fields and reports bad ones', () => {
		const ok = compose(draft({ extraBody: '{"tools": [], "temperature": 1}' }));
		expect(JSON.parse(ok.body ?? '{}')).toMatchObject({ tools: [], temperature: 1 });
		expect(compose(draft({ extraBody: '[1]' })).problems[0]).toMatch(/JSON object/);
		expect(compose(draft({ extraBody: '{' })).problems[0]).toMatch(/not JSON/);
	});

	it('sends a hand-edited body as it is, and says when it is not JSON', () => {
		const c = compose(draft({ rawBody: '{"model": broken' }));
		expect(c.body).toBe('{"model": broken');
		expect(c.notes[0]).toMatch(/not valid JSON/);
		expect(c.problems).toEqual([]);
	});

	it('asks for what is missing', () => {
		const c = compose(draft({ model: '', messages: [{ role: 'user', content: ' ' }] }));
		expect(c.problems).toEqual([
			'Name a model or a template (@name).',
			'Write at least one message.'
		]);
		expect(compose(draft({ interface: 'proxy', provider: '' })).problems).toContain(
			'Choose the provider to proxy to.'
		);
	});

	it('builds the paths of every interface', () => {
		expect(methodAndPath(draft({ interface: 'models' }))).toEqual({
			method: 'GET',
			path: '/v1/models'
		});
		expect(
			methodAndPath(draft({ interface: 'proxy', provider: 'my prov', proxyPath: 'v1/models' }))
		).toEqual({ method: 'POST', path: '/proxy/my%20prov/v1/models' });
		expect(
			methodAndPath(
				draft({ interface: 'custom', customMethod: 'GET', customPath: ' /gw/v1/quota ' })
			)
		).toEqual({ method: 'GET', path: '/gw/v1/quota' });
		expect(compose(draft({ interface: 'custom', customPath: '//evil' })).problems).toContain(
			'The path starts with one /.'
		);
	});
});

describe('helpers', () => {
	it('reads tags', () => {
		expect(parseTags('a=1, b = x=y ,broken, =z')).toEqual({ a: '1', b: 'x=y' });
	});

	it('writes every gateway option', () => {
		const g = {
			...defaultDraft().gateway,
			template: 'writer',
			timeoutMs: '1500',
			unsupportedParams: 'drop' as const,
			promptCache: 'auto' as const,
			audit: false
		};
		expect(gatewayHeaders(g)).toEqual({
			'x-gw-template': 'writer',
			'x-gw-timeout-ms': '1500',
			'x-gw-unsupported-params': 'drop',
			'x-gw-prompt-cache': 'auto'
		});
		expect(gatewayBlock(g)).toEqual({
			template: 'writer',
			timeout_ms: 1500,
			unsupported_params: 'drop',
			cache: { prompt: 'auto' }
		});
	});

	it('chooses where the key goes', () => {
		expect(defaultAuthHeader('anthropic')).toBe('x-api-key');
		expect(defaultAuthHeader('proxy', '/v1/messages')).toBe('x-api-key');
		expect(defaultAuthHeader('native')).toBe('authorization');
	});

	it('makes traceparents', () => {
		expect(newTraceparent((n) => 'a'.repeat(n * 2))).toBe(
			`00-${'a'.repeat(32)}-${'a'.repeat(16)}-01`
		);
		expect(newTraceparent()).toMatch(/^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
	});

	it('restores a stored draft without a key choice', () => {
		const restored = restoreDraft({ model: 'x', keyKind: 'own', gateway: { endUser: 'u' } });
		expect(restored.model).toBe('x');
		expect(restored.keyKind).toBe('temporary');
		expect(restored.gateway.endUser).toBe('u');
		expect(restored.gateway.audit).toBe(true);
		expect(restoreDraft(null)).toEqual(defaultDraft());
	});
});
