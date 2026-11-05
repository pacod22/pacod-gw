import { defaultPriceSources } from '#lib/price-sources.ts';
import { readForm } from '#lib/schema/form.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	objectResource,
	problemOf,
	type KitEvent
} from '#lib/server/resource.ts';

const job = objectResource({
	path: '/price-job',
	noun: 'price job',
	fields: {
		schedule: {
			help: 'A cron expression with five fields: minute, hour, day of month, month, day of week. Days of the week run from 0 (Sunday) to 7 (Sunday again) or by name (mon-fri). Restrict the day of the month or the day of the week, not both.'
		},
		sources: {
			help: 'The sources of earlier releases, as a JSON list: [{"provider_id": "prv_…", "kind": "feed" or "page", "url": "https://…"}]. They are read into price pages when the job is first saved; after that they are kept as they are and no longer read.'
		},
		scan_schedule: {
			help: "When the providers' sites are scanned for pages that list models and prices, in the same form as the schedule. Weekly by default."
		},
		extraction_model: {
			help: 'The model that reads the pages: the name of an active chat model with structured output. Pages are not read until one is set.'
		},
		pages_migrated: { hidden: true }
	}
});

const importFields = () =>
	inputFields('/prices/import', 'post', {
		prices: {
			help: 'A JSON list, token rates per one million tokens: [{"model": "provider/model", "input": 3, "output": 15}].'
		}
	});

export async function load(event: KitEvent) {
	const [base, runs, proposals, providers] = await Promise.all([
		job.load(event),
		event.locals.api.GET('/price-job/runs', { params: { query: { limit: 20 } } }),
		event.locals.api.GET('/price-proposals'),
		event.locals.api.GET('/providers', { params: { query: { limit: 500 } } })
	]);
	// A job without sources finds no prices. Its form starts with LiteLLM's list for the
	// providers that list covers; the page says that these are not saved yet.
	const suggested =
		base.item && base.fields && base.item.sources.length === 0
			? defaultPriceSources(providers.data?.items ?? [])
			: [];
	return {
		...base,
		item: base.item && suggested.length > 0 ? { ...base.item, sources: suggested } : base.item,
		suggestedSources: suggested.length > 0,
		runs: runs.data ?? [],
		runsProblem: runs.response.ok ? null : problemOf(event, runs),
		proposals: proposals.data ?? [],
		proposalsProblem: proposals.response.ok ? null : problemOf(event, proposals),
		importFields: base.fields ? importFields() : null
	};
}

async function decide(event: KitEvent, decision: 'approve' | 'reject') {
	const form = await event.request.formData();
	const id = formText(form, 'id');
	const result = await event.locals.api.POST(`/price-proposals/{id}/${decision}`, {
		params: { path: { id } }
	});
	if (!result.response.ok || !result.data) return failed(event, decision, result, { id });
	return done(
		decision,
		decision === 'approve'
			? `The proposed price for “${result.data.model}” is in effect as version ${result.data.version}.`
			: `The proposed price for “${result.data.model}” was rejected.`,
		{ id }
	);
}

export const actions = {
	...job.actions,

	/** Asks for a run of the price job now. It starts within moments. */
	run: async (event: KitEvent) => {
		const result = await event.locals.api.POST('/price-job/run');
		if (!result.response.ok) return failed(event, 'run', result);
		return done(
			'run',
			'A run of the price job was requested. Its result appears in the list of runs.'
		);
	},
	approve: (event: KitEvent) => decide(event, 'approve'),
	reject: (event: KitEvent) => decide(event, 'reject'),

	/** Takes prices from a list. Each becomes a new price version of its model. */
	import: async (event: KitEvent) => {
		const form = await event.request.formData();
		const fields = importFields();
		const result = await event.locals.api.POST('/prices/import', {
			body: readForm(fields, form) as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'import', result, { fields });
		const counts = Object.entries((result.data.counts ?? {}) as Record<string, unknown>)
			.map(([outcome, count]) => `${count} ${outcome.replaceAll('_', ' ')}`)
			.join(', ');
		return done('import', `The prices were imported${counts ? `: ${counts}` : ''}.`, {
			data: result.data.models
		});
	}
};
