/**
 * Maps an error answer of the management API to what the console shows.
 *
 * The core answers every error as `{"error": {"type", "message", "details"}}`. The analytics
 * service, whose answers the core forwards unchanged, uses `code` in place of `type`.
 */

export type ProblemTone = 'error' | 'warning' | 'info';

export interface ApiProblem {
	/** HTTP status of the answer; 0 when there was no answer. */
	status: number;
	/** The core's error type, such as `validation_failed`. */
	type: string;
	/** What went wrong, in a few words. */
	title: string;
	/** The core's own message as a sentence. */
	message: string;
	/** What the user can do about it. */
	hint: string;
	/** `form`: shown next to the form that was sent. `notice`: shown as a notice on the page. */
	placement: 'form' | 'notice';
	tone: ProblemTone;
	/** The page shows stale data: offer to reload it. */
	reload: boolean;
	/** Messages that belong to one field, by field path. */
	fields: Record<string, string>;
	/** What the error names, from its details: for example the keys that still use a template. */
	lists: { label: string; items: string[] }[];
}

/** The type used when the core did not answer at all. */
export const UNREACHABLE = 'unreachable';

interface Rule {
	title: string;
	hint: string;
	placement: ApiProblem['placement'];
	tone: ProblemTone;
	reload?: boolean;
}

const BY_TYPE: Record<string, Rule> = {
	validation_failed: {
		title: 'The gateway did not accept these values',
		hint: 'Correct the value and send the form again.',
		placement: 'form',
		tone: 'error'
	},
	invalid_request: {
		title: 'The gateway could not read the request',
		hint: 'Check the values and send the form again.',
		placement: 'form',
		tone: 'error'
	},
	invalid_parameter: {
		title: 'A filter has a value the gateway does not accept',
		hint: 'Correct the filter and apply it again.',
		placement: 'form',
		tone: 'error'
	},
	conflict: {
		title: 'This change conflicts with the current state',
		hint: 'Reload the page to see the current state, then try again.',
		placement: 'form',
		tone: 'warning',
		reload: true
	},
	version_mismatch: {
		title: 'Someone else changed this object in the meantime',
		hint: 'Reload the page to get the current version, then make your change again.',
		placement: 'form',
		tone: 'warning',
		reload: true
	},
	forbidden: {
		title: 'Your role does not allow this',
		hint: 'Ask an administrator of the platform or of the organisation for the role you need.',
		placement: 'notice',
		tone: 'warning'
	},
	unauthorized: {
		title: 'You are not signed in',
		hint: 'Sign in again.',
		placement: 'notice',
		tone: 'warning'
	},
	not_found: {
		title: 'Not found',
		hint: 'It may have been deleted. Reload the page to see what exists now.',
		placement: 'notice',
		tone: 'warning',
		reload: true
	},
	too_many_attempts: {
		title: 'Too many attempts',
		hint: 'Wait a few minutes before you try again.',
		placement: 'form',
		tone: 'warning'
	},
	unavailable: {
		title: 'This part of the gateway is not available',
		hint: 'Configuration and traffic are not affected. Try again later.',
		placement: 'notice',
		tone: 'info'
	},
	email_unavailable: {
		title: 'This installation does not send email',
		hint: 'An administrator can set up email (PGW_SMTP_HOST), mark the address as confirmed, or give the user a new password.',
		placement: 'notice',
		tone: 'warning'
	},
	email_failed: {
		title: 'The email was not sent',
		hint: 'Check the email settings of the gateway with "pacod-gw-core email-check". Until they work, mark the address as confirmed or give the user a new password.',
		placement: 'notice',
		tone: 'error'
	},
	[UNREACHABLE]: {
		title: 'The gateway does not answer',
		hint: 'Check that the core is running and that GATEWAY_API_URL points to its management listener.',
		placement: 'notice',
		tone: 'error'
	},
	internal: {
		title: 'The gateway reported an internal error',
		hint: 'Try again. If it keeps failing, look at the log of the core.',
		placement: 'notice',
		tone: 'error'
	}
};

const BY_STATUS: Record<number, string> = {
	400: 'invalid_request',
	401: 'unauthorized',
	403: 'forbidden',
	404: 'not_found',
	409: 'conflict',
	422: 'validation_failed',
	429: 'too_many_attempts',
	503: 'unavailable'
};

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Turns the core's lower-case fragments into a sentence. */
export function sentence(text: string): string {
	const trimmed = text.trim();
	if (!trimmed) return '';
	// A message that starts with a field name such as `reporting_currency` keeps its spelling.
	const startsWithIdentifier = /^[a-z0-9]+_[a-z0-9_]+\b/.test(trimmed);
	const first = startsWithIdentifier ? trimmed : trimmed[0].toUpperCase() + trimmed.slice(1);
	return /[.!?…]$/.test(first) ? first : `${first}.`;
}

/** Reads `{"error": {...}}`; anything else yields an empty envelope. */
function envelope(body: unknown): { type: string; message: string; details: unknown } {
	const inner = isRecord(body) && isRecord(body.error) ? body.error : {};
	const type =
		typeof inner.type === 'string' ? inner.type : typeof inner.code === 'string' ? inner.code : '';
	const message = typeof inner.message === 'string' ? inner.message : '';
	return { type, message, details: inner.details };
}

/**
 * Finds the fields an error is about.
 *
 * The core does not name the field in a machine-readable way yet, so besides the shapes of
 * `details` that would carry it, the message is searched for one of the form's field names:
 * as its first word (`name must be …`) or in backticks (``missing field `name` ``).
 */
export function fieldErrors(
	message: string,
	details: unknown,
	fieldNames: readonly string[]
): Record<string, string> {
	const out: Record<string, string> = {};
	const known = new Set(fieldNames);
	// A field may be named by its path (`settings.locked`) or by its last segment (`locked`).
	const byLeaf = new Map<string, string>();
	for (const name of fieldNames) {
		const leaf = name.split('.').pop() ?? name;
		if (!byLeaf.has(leaf)) byLeaf.set(leaf, name);
	}
	const lookup = (name: string): string | undefined =>
		known.has(name) ? name : byLeaf.get(name.split('.').pop() ?? name);

	const add = (name: unknown, text: unknown) => {
		if (typeof name !== 'string') return;
		const path = lookup(name) ?? (fieldNames.length === 0 ? name : undefined);
		if (path) out[path] = sentence(typeof text === 'string' && text ? text : message);
	};

	if (isRecord(details)) {
		if (isRecord(details.fields)) {
			for (const [name, text] of Object.entries(details.fields)) add(name, text);
		}
		add(details.field ?? details.path, details.message);
	} else if (Array.isArray(details)) {
		for (const entry of details) {
			if (isRecord(entry)) add(entry.field ?? entry.path, entry.message);
		}
	}
	if (Object.keys(out).length > 0 || !message) return out;

	const quoted = [...message.matchAll(/`([^`]+)`/g)]
		.map((m) => lookup(m[1]))
		.filter((name): name is string => name !== undefined);
	const first = lookup(message.trim().split(/[\s:]/, 1)[0] ?? '');
	const candidates = first ? [first] : [...new Set(quoted)];
	if (candidates.length === 1) out[candidates[0]] = sentence(message);
	return out;
}

/**
 * Describes a failed call.
 *
 * @param status HTTP status of the answer, 0 when there was none
 * @param body the parsed answer
 * @param fieldNames the paths of the form's fields, to attach the message to one of them
 */
export function toProblem(
	status: number,
	body: unknown,
	fieldNames: readonly string[] = []
): ApiProblem {
	const { type: given, message, details } = envelope(body);
	const fallback = BY_STATUS[status] ?? (status >= 500 || status === 0 ? 'internal' : '');
	// The status decides when the type is not one the console knows, for example the
	// analytics service's `storage_unavailable` (503).
	const type = given || fallback || 'error';
	const rule: Rule = BY_TYPE[given] ??
		BY_TYPE[fallback] ?? {
			title: 'The request failed',
			hint: 'Try again.',
			placement: 'notice',
			tone: 'error'
		};
	const fields =
		rule.placement === 'form' && status !== 409 ? fieldErrors(message, details, fieldNames) : {};
	const lists = isRecord(details)
		? Object.entries(details)
				.filter(
					(entry): entry is [string, string[]] =>
						Array.isArray(entry[1]) &&
						entry[1].length > 0 &&
						entry[1].every((item) => typeof item === 'string')
				)
				.map(([label, items]) => ({ label: label.replaceAll('_', ' '), items }))
		: [];
	return {
		status,
		type,
		title: rule.title,
		message: sentence(message) || `The gateway answered with status ${status}.`,
		hint: rule.hint,
		placement: rule.placement,
		tone: rule.tone,
		reload: rule.reload ?? false,
		fields,
		lists
	};
}

/** A problem found by the console itself, before anything was sent. */
export function localProblem(message: string, fields: Record<string, string> = {}): ApiProblem {
	return {
		status: 400,
		type: 'validation_failed',
		title: 'The form is incomplete',
		message,
		hint: '',
		placement: 'form',
		tone: 'error',
		reload: false,
		fields,
		lists: []
	};
}
