/**
 * The message composer of the Testing pages: its state, and how a request is built from it for
 * each interface of the gateway. Pure functions, so that every interface's shape is tested.
 */

export type Interface = 'openai' | 'anthropic' | 'native' | 'proxy' | 'models' | 'custom';
export type Method = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
export type Role = 'user' | 'assistant';
export type AuthHeader = 'authorization' | 'x-api-key' | 'x-goog-api-key';

export interface ChatMessage {
	role: Role;
	content: string;
}

export interface HeaderRow {
	name: string;
	value: string;
}

/** The gateway's own options; each becomes an x-gw-* header or a field of the body's gateway block. */
export interface GatewayOptions {
	template: string;
	endUser: string;
	session: string;
	trace: string;
	agentId: string;
	userId: string;
	task: string;
	complexity: '' | 'low' | 'medium' | 'high';
	/** `key=value, key=value` */
	tags: string;
	timeoutMs: string;
	retries: string;
	unsupportedParams: '' | 'drop' | 'reject';
	promptCache: '' | 'off' | 'passthrough' | 'auto';
	audit: boolean;
}

export interface Draft {
	org: string;
	ws: string;
	keyKind: 'temporary' | 'own';
	interface: Interface;
	/** Proxy mode: the provider's name and the path below it. */
	provider: string;
	proxyPath: string;
	customMethod: Method;
	customPath: string;
	model: string;
	system: string;
	messages: ChatMessage[];
	maxTokens: string;
	temperature: string;
	topP: string;
	/** One stop sequence per line. */
	stop: string;
	stream: boolean;
	includeUsage: boolean;
	gateway: GatewayOptions;
	/** Where the gateway options go. The body's gateway block exists in the OpenAI and native interfaces. */
	gatewayIn: 'headers' | 'body';
	traceparent: string;
	extraHeaders: HeaderRow[];
	authHeader: AuthHeader;
	/** A JSON object whose fields are added to the top of the body: tools, response_format, … */
	extraBody: string;
	/** The body as edited by hand; null while the form builds it. */
	rawBody: string | null;
	timeoutS: number;
}

export const INTERFACES: { id: Interface; label: string; path: string }[] = [
	{ id: 'openai', label: 'OpenAI chat completions', path: '/v1/chat/completions' },
	{ id: 'anthropic', label: 'Anthropic messages', path: '/v1/messages' },
	{ id: 'native', label: 'Native chat', path: '/gw/v1/chat' },
	{ id: 'proxy', label: 'Proxy to a provider', path: '/proxy/{provider}/…' },
	{ id: 'models', label: 'Model list', path: '/v1/models' },
	{ id: 'custom', label: 'Any path', path: '…' }
];

export const ANTHROPIC_VERSION = '2023-06-01';

export function defaultDraft(org = '', ws = ''): Draft {
	return {
		org,
		ws,
		keyKind: 'temporary',
		interface: 'openai',
		provider: '',
		proxyPath: '/v1/chat/completions',
		customMethod: 'POST',
		customPath: '/v1/chat/completions',
		model: '',
		system: '',
		messages: [{ role: 'user', content: 'Reply with the single word: pong' }],
		maxTokens: '',
		temperature: '',
		topP: '',
		stop: '',
		stream: false,
		includeUsage: true,
		gateway: {
			template: '',
			endUser: '',
			session: '',
			trace: '',
			agentId: '',
			userId: '',
			task: '',
			complexity: '',
			tags: '',
			timeoutMs: '',
			retries: '',
			unsupportedParams: '',
			promptCache: '',
			audit: true
		},
		gatewayIn: 'headers',
		traceparent: '',
		extraHeaders: [],
		authHeader: 'authorization',
		extraBody: '',
		rawBody: null,
		timeoutS: 120
	};
}

/** A draft restored from a stored message, with anything missing filled from the defaults. */
export function restoreDraft(saved: unknown): Draft {
	const base = defaultDraft();
	if (!saved || typeof saved !== 'object') return base;
	const s = saved as Partial<Draft>;
	return {
		...base,
		...s,
		gateway: { ...base.gateway, ...(s.gateway ?? {}) },
		messages: Array.isArray(s.messages) ? s.messages : base.messages,
		extraHeaders: Array.isArray(s.extraHeaders) ? s.extraHeaders : [],
		// A restored draft never carries a key: the temporary key is the safe default.
		keyKind: 'temporary'
	};
}

/** The dialect a body is written in. */
export function dialectOf(d: Draft): 'openai' | 'anthropic' | 'native' | null {
	switch (d.interface) {
		case 'openai':
		case 'anthropic':
		case 'native':
			return d.interface;
		case 'proxy':
			return d.proxyPath.split('?')[0].endsWith('/messages') ? 'anthropic' : 'openai';
		default:
			return null;
	}
}

/** Where the key goes by default: Anthropic's SDK sends it in x-api-key. */
export function defaultAuthHeader(i: Interface, proxyPath = ''): AuthHeader {
	if (i === 'anthropic') return 'x-api-key';
	if (i === 'proxy' && proxyPath.split('?')[0].endsWith('/messages')) return 'x-api-key';
	return 'authorization';
}

export function methodAndPath(d: Draft): { method: Method; path: string } {
	switch (d.interface) {
		case 'openai':
			return { method: 'POST', path: '/v1/chat/completions' };
		case 'anthropic':
			return { method: 'POST', path: '/v1/messages' };
		case 'native':
			return { method: 'POST', path: '/gw/v1/chat' };
		case 'models':
			return { method: 'GET', path: '/v1/models' };
		case 'proxy': {
			const below = d.proxyPath.startsWith('/') ? d.proxyPath : `/${d.proxyPath}`;
			return { method: 'POST', path: `/proxy/${encodeURIComponent(d.provider.trim())}${below}` };
		}
		case 'custom':
			return { method: d.customMethod, path: d.customPath.trim() };
	}
}

function number(text: string): number | undefined {
	const t = text.trim();
	if (t === '') return undefined;
	const n = Number(t);
	return Number.isFinite(n) ? n : undefined;
}

function lines(text: string): string[] {
	return text
		.split('\n')
		.map((s) => s.trim())
		.filter(Boolean);
}

/** `purpose=writer, site=pacod` as an object. */
export function parseTags(text: string): Record<string, string> {
	const out: Record<string, string> = {};
	for (const pair of text.split(',')) {
		const [key, ...rest] = pair.split('=');
		if (key?.trim() && rest.length) out[key.trim()] = rest.join('=').trim();
	}
	return out;
}

/** The gateway options as x-gw-* headers. */
export function gatewayHeaders(g: GatewayOptions): Record<string, string> {
	const out: Record<string, string> = {};
	const set = (name: string, value: string) => {
		if (value.trim()) out[name] = value.trim();
	};
	set('x-gw-template', g.template);
	set('x-gw-end-user', g.endUser);
	set('x-gw-session', g.session);
	set('x-gw-trace', g.trace);
	set('x-gw-agent-id', g.agentId);
	set('x-gw-user-id', g.userId);
	set('x-gw-task', g.task);
	set('x-gw-complexity', g.complexity);
	set('x-gw-tags', g.tags);
	set('x-gw-timeout-ms', g.timeoutMs);
	set('x-gw-retries', g.retries);
	set('x-gw-unsupported-params', g.unsupportedParams);
	set('x-gw-prompt-cache', g.promptCache);
	if (g.audit) out['x-gw-audit'] = 'true';
	return out;
}

/** The gateway options as the body's `gateway` block. */
export function gatewayBlock(g: GatewayOptions): Record<string, unknown> {
	const out: Record<string, unknown> = {};
	const set = (name: string, value: unknown) => {
		if (value !== undefined && value !== '') out[name] = value;
	};
	set('template', g.template.trim());
	set('end_user', g.endUser.trim());
	set('session_id', g.session.trim());
	set('trace_id', g.trace.trim());
	set('agent_id', g.agentId.trim());
	set('user_id', g.userId.trim());
	set('task', g.task.trim());
	set('complexity', g.complexity);
	const tags = parseTags(g.tags);
	if (Object.keys(tags).length) out.tags = tags;
	set('timeout_ms', number(g.timeoutMs));
	const retries = number(g.retries);
	if (retries !== undefined) out.retry = { max_attempts: retries };
	set('unsupported_params', g.unsupportedParams);
	if (g.promptCache) out.cache = { prompt: g.promptCache };
	if (g.audit) out.audit = true;
	return out;
}

const blockAllowed = (d: Draft) => d.interface === 'openai' || d.interface === 'native';

/** The body the form describes, as an object; null for interfaces without one. */
export function buildBody(d: Draft): Record<string, unknown> | null {
	const dialect = dialectOf(d);
	if (!dialect) return null;
	const body: Record<string, unknown> = { model: d.model.trim() };
	const said = d.messages.filter((m) => m.content.trim() !== '');
	const maxTokens = number(d.maxTokens);
	const stops = lines(d.stop);

	if (dialect === 'openai') {
		body.messages = [
			...(d.system.trim() ? [{ role: 'system', content: d.system }] : []),
			...said.map((m) => ({ role: m.role, content: m.content }))
		];
		if (maxTokens !== undefined) body.max_tokens = maxTokens;
		if (stops.length) body.stop = stops;
	} else if (dialect === 'anthropic') {
		// Anthropic requires max_tokens.
		body.max_tokens = maxTokens ?? 1024;
		if (d.system.trim()) body.system = d.system;
		body.messages = said.map((m) => ({ role: m.role, content: m.content }));
		if (stops.length) body.stop_sequences = stops;
	} else {
		if (d.system.trim()) body.system = [{ type: 'text', text: d.system }];
		body.messages = said.map((m) => ({
			role: m.role,
			content: [{ type: 'text', text: m.content }]
		}));
		if (maxTokens !== undefined) body.max_output_tokens = maxTokens;
		if (stops.length) body.stop = stops;
	}
	const temperature = number(d.temperature);
	if (temperature !== undefined) body.temperature = temperature;
	const topP = number(d.topP);
	if (topP !== undefined) body.top_p = topP;
	if (d.stream) {
		body.stream = true;
		if (dialect === 'openai' && d.includeUsage) body.stream_options = { include_usage: true };
	}
	if (d.gatewayIn === 'body' && blockAllowed(d)) {
		const block = gatewayBlock(d.gateway);
		if (Object.keys(block).length) body.gateway = block;
	}
	return body;
}

/** The extra body fields, or why they cannot be used. */
export function extraFields(text: string): {
	value: Record<string, unknown>;
	error: string | null;
} {
	if (!text.trim()) return { value: {}, error: null };
	try {
		const parsed: unknown = JSON.parse(text);
		if (parsed && typeof parsed === 'object' && !Array.isArray(parsed))
			return { value: parsed as Record<string, unknown>, error: null };
		return { value: {}, error: 'The additional fields must be a JSON object.' };
	} catch (e) {
		return { value: {}, error: `The additional fields are not JSON: ${(e as Error).message}` };
	}
}

/** Headers set by the tester itself; the form does not offer them. */
export const RESERVED_HEADERS = new Set([
	'authorization',
	'x-api-key',
	'x-goog-api-key',
	'proxy-authorization',
	'cookie',
	'host',
	'content-length',
	'transfer-encoding',
	'connection',
	'upgrade',
	'te'
]);

export interface Composed {
	method: Method;
	path: string;
	headers: Record<string, string>;
	body: string | null;
	authHeader: AuthHeader;
	/** Things that keep the message from being sent. */
	problems: string[];
	/** Things worth knowing that do not stop it. */
	notes: string[];
}

/** Everything the tester needs to send the message. */
export function compose(d: Draft): Composed {
	const { method, path } = methodAndPath(d);
	const problems: string[] = [];
	const notes: string[] = [];
	if (!path.startsWith('/') || path.startsWith('//')) problems.push('The path starts with one /.');
	if (d.interface === 'proxy' && !d.provider.trim())
		problems.push('Choose the provider to proxy to.');

	const headers: Record<string, string> = {};
	const dialect = dialectOf(d);
	if (dialect === 'anthropic') headers['anthropic-version'] = ANTHROPIC_VERSION;
	if (d.gatewayIn === 'headers' || !blockAllowed(d))
		Object.assign(headers, gatewayHeaders(d.gateway));
	else if (d.gateway.audit) headers['x-gw-audit'] = 'true';
	if (d.traceparent.trim()) headers.traceparent = d.traceparent.trim();
	for (const row of d.extraHeaders) {
		const name = row.name.trim();
		if (!name) continue;
		if (RESERVED_HEADERS.has(name.toLowerCase())) {
			problems.push(`The header ${name} is set by the tester; choose where the key goes instead.`);
			continue;
		}
		if (/[\r\n]/.test(row.value)) problems.push(`The value of ${name} holds a line break.`);
		// A header of the same name from the form is replaced, whatever its case.
		for (const key of Object.keys(headers))
			if (key.toLowerCase() === name.toLowerCase()) delete headers[key];
		headers[name] = row.value;
	}

	let body: string | null = null;
	if (d.rawBody !== null) {
		body = d.rawBody === '' && method === 'GET' ? null : d.rawBody;
		if (body !== null) {
			try {
				JSON.parse(body);
			} catch {
				notes.push('The body is not valid JSON. It is sent as it is, and the preview is skipped.');
			}
		}
	} else {
		const built = buildBody(d);
		if (built) {
			const extra = extraFields(d.extraBody);
			if (extra.error) problems.push(extra.error);
			if (!built.model) problems.push('Name a model or a template (@name).');
			if (d.messages.every((m) => !m.content.trim())) problems.push('Write at least one message.');
			body = JSON.stringify({ ...built, ...extra.value }, null, 2);
		}
	}
	return { method, path, headers, body, authHeader: d.authHeader, problems, notes };
}

/** A fresh W3C traceparent, so that the call can be found in a tracing system. */
export function newTraceparent(random: (bytes: number) => string = randomHex): string {
	return `00-${random(16)}-${random(8)}-01`;
}

function randomHex(bytes: number): string {
	const data = new Uint8Array(bytes);
	crypto.getRandomValues(data);
	return Array.from(data, (b) => b.toString(16).padStart(2, '0')).join('');
}
