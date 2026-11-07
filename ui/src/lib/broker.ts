/**
 * What an organisation offers on pacod-broker, as the Broker page chooses it: which of its
 * granted models and templates are ticked, with whom each is shared, and the change the page
 * posts. Only rows whose offer the form changes are sent, so that a change made elsewhere
 * after the page was read (on a template's page, say) stays.
 */

import type { Schemas } from '#lib/api/types.ts';

export type BrokerOffer = Schemas['BrokerOffer'];
export type OfferedModel = Schemas['OfferedModel'];
export type OfferedTemplate = Schemas['OfferedTemplate'];
export type BrokerOfferChange = Schemas['BrokerOfferChange'];

export const VISIBILITIES = [
	{ value: 'organisation', label: 'The organisation’s members' },
	{ value: 'platform', label: 'Everyone signed in' },
	{ value: 'public', label: 'Anyone' }
] as const;

/** A newly ticked row is found by the organisation's own members, as the template switch has it. */
export const DEFAULT_VISIBILITY = 'organisation';

/** The models that take calls; another one is not offered. */
const IN_SERVICE = ['active', 'deprecated'];

/** Why a model cannot be ticked, or null. */
export function modelBlocked(m: Pick<OfferedModel, 'key' | 'reached_by' | 'state'>): string | null {
	if (!m.key) return 'its name is not a broker key (lower case: a-z, 0-9, - _ . : /)';
	if (m.reached_by.length === 0) return 'no workspace of the organisation may call it';
	if (!IN_SERVICE.includes(m.state)) return `it is ${m.state}`;
	return null;
}

/** Why a template cannot be ticked, or null. */
export function templateBlocked(t: Pick<OfferedTemplate, 'status' | 'reached_by'>): string | null {
	if (t.status !== 'active') return `it is ${t.status}`;
	if (t.reached_by.length === 0) return 'every workspace has a template of this name of its own';
	return null;
}

/** One row as the page shows it: what it offers now. */
export interface ShownRow {
	id: string;
	offer: BrokerOffer | null;
}

/**
 * The rows the form may change. A row that cannot be offered is left out unless it is
 * offered now: it can still be withdrawn, and never changes by being left alone.
 */
export function editable<T extends { offer?: BrokerOffer | null }>(
	rows: T[],
	id: (row: T) => string,
	blocked: (row: T) => string | null
): ShownRow[] {
	return rows
		.filter((r) => r.offer || !blocked(r))
		.map((r) => ({ id: id(r), offer: r.offer ?? null }));
}

/** What the form says of one row. */
export interface Choice {
	id: string;
	ticked: boolean;
	visibility: string;
	summary: string;
}

/** The form's choices for `shown`: `name` holds the ticked ids, each row its own fields. */
export function choicesFrom(
	form: FormData,
	name: 'model' | 'template',
	shown: ShownRow[]
): Choice[] {
	const ticked = new Set(form.getAll(name).map(String));
	const text = (field: string) => {
		const v = form.get(field);
		return typeof v === 'string' ? v : '';
	};
	return shown.map((r) => ({
		id: r.id,
		ticked: ticked.has(r.id),
		visibility: text(`visibility:${r.id}`) || DEFAULT_VISIBILITY,
		summary: text(`summary:${r.id}`).trim()
	}));
}

function offerOf(c: Choice): BrokerOffer | null {
	if (!c.ticked) return null;
	return c.summary
		? { visibility: c.visibility, summary: c.summary }
		: { visibility: c.visibility };
}

function same(a: BrokerOffer | null, b: BrokerOffer | null): boolean {
	if (!a || !b) return a === b;
	return a.visibility === b.visibility && (a.summary ?? '') === (b.summary ?? '');
}

/** The rows whose offer changes, each with its new offer; null withdraws it. */
export function offerChanges(
	shown: ShownRow[],
	chosen: Choice[]
): { id: string; offer: BrokerOffer | null }[] {
	const was = new Map(shown.map((r) => [r.id, r.offer]));
	return chosen
		.filter((c) => was.has(c.id))
		.map((c) => ({ id: c.id, offer: offerOf(c) }))
		.filter((c) => !same(was.get(c.id) ?? null, c.offer));
}

/** The body of the change: models by id, templates by id. */
export function changeBody(
	models: { id: string; offer: BrokerOffer | null }[],
	templates: { id: string; offer: BrokerOffer | null }[]
): BrokerOfferChange {
	return {
		models: models.map((m) => ({ model_id: m.id, offer: m.offer })),
		templates: templates.map((t) => ({ id: t.id, offer: t.offer }))
	};
}

/** "2 offered, 1 withdrawn, 1 changed", for the answer to a change. */
export function changeSummary(
	shown: ShownRow[],
	changes: { id: string; offer: BrokerOffer | null }[]
): string {
	const was = new Map(shown.map((r) => [r.id, r.offer]));
	let offered = 0;
	let withdrawn = 0;
	let changed = 0;
	for (const c of changes) {
		if (!c.offer) withdrawn++;
		else if (was.get(c.id)) changed++;
		else offered++;
	}
	const parts = [
		offered ? `${offered} offered` : '',
		withdrawn ? `${withdrawn} withdrawn` : '',
		changed ? `${changed} changed` : ''
	].filter(Boolean);
	return parts.join(', ');
}
