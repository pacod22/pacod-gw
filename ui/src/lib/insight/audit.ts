/**
 * The audit record of one call: its four parts, and what the gateway changed between the
 * request it received and the request it sent.
 */

export interface AuditPart {
	method: string;
	url: string;
	status: number;
	headers: [string, string][];
	body: string;
	/** `utf8`: the body as text. `base64`: binary. `ref`: the body is that of `same_as`. */
	body_encoding: string;
	body_bytes: number;
	/** The part whose body is identical, such as `provider_responses[0]`. */
	same_as: string;
	truncated: boolean;
}

export interface NamedPart {
	/** The name other parts refer to: `client_request`, `provider_requests[0]`. */
	name: string;
	title: string;
	part: AuditPart;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function asPart(value: unknown): AuditPart | null {
	if (!isRecord(value)) return null;
	const headers = Array.isArray(value.headers)
		? value.headers
				.filter((h): h is [string, string] => Array.isArray(h) && h.length >= 2)
				.map((h): [string, string] => [String(h[0]), String(h[1])])
		: [];
	return {
		method: String(value.method ?? ''),
		url: String(value.url ?? ''),
		status: typeof value.status === 'number' ? value.status : 0,
		headers,
		body: typeof value.body === 'string' ? value.body : '',
		body_encoding: String(value.body_encoding ?? 'utf8'),
		body_bytes: typeof value.body_bytes === 'number' ? value.body_bytes : 0,
		same_as: String(value.same_as ?? ''),
		truncated: value.truncated === true
	};
}

/** The four kinds of part in the order of the call, one entry per attempt. */
export function partsOf(record: unknown): NamedPart[] {
	if (!isRecord(record)) return [];
	const out: NamedPart[] = [];
	const add = (name: string, title: string, value: unknown) => {
		const part = asPart(value);
		if (part) out.push({ name, title, part });
	};
	const list = (key: string) => (Array.isArray(record[key]) ? (record[key] as unknown[]) : []);
	add('client_request', 'Request from the client', record.client_request);
	const attempts = Math.max(list('provider_requests').length, list('provider_responses').length);
	for (let i = 0; i < attempts; i++) {
		const suffix = attempts > 1 ? `, attempt ${i + 1}` : '';
		add(
			`provider_requests[${i}]`,
			`Request to the provider${suffix}`,
			list('provider_requests')[i]
		);
		add(
			`provider_responses[${i}]`,
			`Response of the provider${suffix}`,
			list('provider_responses')[i]
		);
	}
	add('client_response', 'Response to the client', record.client_response);
	return out;
}

/** The body of a part as text to show; a body that refers to another part is taken from there. */
export function bodyText(parts: NamedPart[], named: NamedPart): { text: string; note: string } {
	const { part } = named;
	if (part.body_encoding === 'ref' || (part.same_as && !part.body)) {
		const target = parts.find((p) => p.name === part.same_as);
		return {
			text: target ? bodyText(parts, target).text : '',
			note: `The same ${part.body_bytes} bytes as ${target?.title.toLowerCase() ?? part.same_as}.`
		};
	}
	if (part.body_encoding === 'base64') {
		return { text: part.body, note: `Binary, ${part.body_bytes} bytes, shown as base64.` };
	}
	return {
		text: pretty(part.body),
		note: part.truncated ? 'The body was cut at the size limit for audit records.' : ''
	};
}

/** JSON is shown indented; anything else as it is. */
export function pretty(body: string): string {
	try {
		return JSON.stringify(JSON.parse(body), null, 2);
	} catch {
		return body;
	}
}

export interface DiffLine {
	type: 'same' | 'removed' | 'added';
	text: string;
}

/** A line difference by longest common subsequence: what was removed, added, or kept. */
export function lineDiff(before: string[], after: string[]): DiffLine[] {
	const n = before.length;
	const m = after.length;
	// lcs[i][j]: length of the common subsequence of before[i..] and after[j..].
	const lcs: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
	for (let i = n - 1; i >= 0; i--) {
		for (let j = m - 1; j >= 0; j--) {
			lcs[i][j] =
				before[i] === after[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
		}
	}
	const out: DiffLine[] = [];
	let i = 0;
	let j = 0;
	while (i < n && j < m) {
		if (before[i] === after[j]) {
			out.push({ type: 'same', text: before[i] });
			i++;
			j++;
		} else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
			out.push({ type: 'removed', text: before[i++] });
		} else {
			out.push({ type: 'added', text: after[j++] });
		}
	}
	while (i < n) out.push({ type: 'removed', text: before[i++] });
	while (j < m) out.push({ type: 'added', text: after[j++] });
	return out;
}

/** What the gateway changed: the client's request body against the first provider request. */
export function requestDiff(parts: NamedPart[]): DiffLine[] | null {
	const client = parts.find((p) => p.name === 'client_request');
	const provider = parts.find((p) => p.name === 'provider_requests[0]');
	if (!client || !provider) return null;
	if (client.part.body_encoding === 'base64' || provider.part.body_encoding === 'base64')
		return null;
	return lineDiff(
		bodyText(parts, client).text.split('\n'),
		bodyText(parts, provider).text.split('\n')
	);
}

/** The simple values of an object as label and text; nested objects as compact JSON. */
export function factsOf(value: unknown): { label: string; text: string }[] {
	if (!isRecord(value)) return [];
	return Object.entries(value).map(([key, v]) => ({
		label: key.replaceAll('_', ' '),
		text:
			v === null || v === undefined || v === ''
				? '–'
				: Array.isArray(v)
					? v.length === 0
						? 'None'
						: v
								.map((item) => (typeof item === 'object' ? JSON.stringify(item) : String(item)))
								.join('; ')
					: typeof v === 'object'
						? JSON.stringify(v)
						: String(v)
	}));
}
