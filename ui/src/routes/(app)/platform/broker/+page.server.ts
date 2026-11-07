import { error, fail } from '@sveltejs/kit';
import { hasPlatformRole } from '#lib/access.ts';
import { localProblem } from '#lib/api/errors.ts';
import {
	changeBody,
	changeSummary,
	choicesFrom,
	offerChanges,
	type ShownRow
} from '#lib/broker.ts';
import { everyModel, organisationOptions } from '#lib/server/options.ts';
import {
	appliedNote,
	done,
	failed,
	formText,
	problemOf,
	type KitEvent
} from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

/** How many linked models the page asks the broker about at once. */
const MAX_LINKED = 50;

/** The gateway's own organisation, for its own calls: it offers nothing. */
const SYSTEM_ORG = 'org_system';

export interface LinkedModel {
	id: string;
	name: string;
	state: string;
	resource: string;
	score: number | null;
	label: string;
	price: string;
	live: string;
}

function priceOf(v: Record<string, unknown> | undefined): string {
	const p = v?.price_now as
		{ currency?: string; rates?: Record<string, number> } | null | undefined;
	if (!p?.rates) return '';
	const r = p.rates;
	const parts = [
		r.input !== undefined ? `in ${r.input}` : '',
		r.output !== undefined ? `out ${r.output}` : ''
	]
		.filter(Boolean)
		.join(' · ');
	return parts ? `${p.currency ?? ''} ${parts}` : '';
}

/** What the organisation chosen with `?org=` offers, or why it cannot be read. */
async function offersOf(event: KitEvent, org: string) {
	if (!org) return { offer: null, offerProblem: null };
	const r = await event.locals.api.GET('/orgs/{org}/broker-offer', { params: { path: { org } } });
	return {
		offer: r.data ?? null,
		offerProblem: r.response.ok ? null : problemOf(event, r)
	};
}

export const load: PageServerLoad = async (event) => {
	const principal = event.locals.principal;
	if (!principal || !hasPlatformRole(principal)) {
		error(403, 'The broker page is for platform roles.');
	}
	const org = event.url.searchParams.get('org') ?? '';
	const [status, models, organisations, offers] = await Promise.all([
		event.locals.api.GET('/broker/status'),
		everyModel(event.locals.api),
		organisationOptions(event),
		offersOf(event, org)
	]);
	const items = models.filter(
		(m) => typeof m.broker_resource_id === 'string' && m.broker_resource_id !== ''
	);
	const configured = Boolean((status.data as { configured?: boolean } | undefined)?.configured);
	const linked: LinkedModel[] = await Promise.all(
		items.slice(0, MAX_LINKED).map(async (m) => {
			const resource = String(m.broker_resource_id);
			const base: LinkedModel = {
				id: String(m.id),
				name: String(m.name),
				state: String(m.state ?? ''),
				resource,
				score: null,
				label: '',
				price: '',
				live: ''
			};
			if (!configured) return base;
			const r = await event.locals.api.GET('/broker/resources/{id}', {
				params: { path: { id: resource } }
			});
			const v = r.data as Record<string, unknown> | undefined;
			if (!r.response.ok || !v) return { ...base, live: 'not on the broker now' };
			const rep = v.reputation as { score?: number; label?: string } | null | undefined;
			const live = v.live as { status?: string } | undefined;
			return {
				...base,
				score: typeof rep?.score === 'number' ? rep.score : null,
				label: rep?.label ?? 'new',
				price: priceOf(v),
				live: live?.status ?? ''
			};
		})
	);
	return {
		status: status.data ?? null,
		problem: status.response.ok ? null : problemOf(event, status),
		linked,
		more: Math.max(0, items.length - MAX_LINKED),
		organisations: organisations.filter((o) => o.value !== SYSTEM_ORG),
		org,
		...offers,
		// Model offers live on the organisation's grants, which only the platform changes.
		canOffer: principal.platform_admin === true
	};
};

function shownRows(form: FormData): { models: ShownRow[]; templates: ShownRow[] } | null {
	try {
		const v = JSON.parse(formText(form, 'shown')) as {
			models?: ShownRow[];
			templates?: ShownRow[];
		};
		if (!Array.isArray(v.models) || !Array.isArray(v.templates)) return null;
		return { models: v.models, templates: v.templates };
	} catch {
		return null;
	}
}

export const actions = {
	/** Offers or withdraws the rows whose tick, visibility or summary the form changed. */
	offer: async (event: KitEvent) => {
		// A load guard does not cover actions.
		if (event.locals.principal?.platform_admin !== true) {
			return fail(403, {
				action: 'offer',
				ok: false,
				problem: { ...localProblem('Only a platform admin changes what is offered.'), status: 403 }
			});
		}
		const form = await event.request.formData();
		const org = formText(form, 'org');
		const shown = shownRows(form);
		if (!org || !shown) {
			return fail(400, {
				action: 'offer',
				ok: false,
				problem: localProblem('The page was not read in full. Load it again and try once more.')
			});
		}
		const models = offerChanges(shown.models, choicesFrom(form, 'model', shown.models));
		const templates = offerChanges(shown.templates, choicesFrom(form, 'template', shown.templates));
		if (models.length === 0 && templates.length === 0) {
			return done('offer', 'Nothing changed.');
		}
		const result = await event.locals.api.POST('/orgs/{org}/broker-offer', {
			params: { path: { org } },
			body: changeBody(models, templates)
		});
		if (!result.response.ok) return failed(event, 'offer', result);
		const said = [
			models.length ? `Models: ${changeSummary(shown.models, models)}.` : '',
			templates.length ? `Templates: ${changeSummary(shown.templates, templates)}.` : ''
		]
			.filter(Boolean)
			.join(' ');
		return done(
			'offer',
			`${said} gw-broker announces the change within seconds.${appliedNote(result.response)}`
		);
	}
};
