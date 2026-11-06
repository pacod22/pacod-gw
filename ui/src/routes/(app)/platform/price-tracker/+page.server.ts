import { readForm } from '#lib/schema/form.ts';
import { allModels } from '#lib/server/options.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	objectResource,
	problemOf,
	type KitEvent
} from '#lib/server/resource.ts';

const CRON =
	'A cron expression with five fields: minute, hour, day of month, month, day of week. Days of the week run from 0 (Sunday) to 7 (Sunday again) or by name (mon-fri). Restrict the day of the month or the day of the week, not both.';

const job = objectResource({
	path: '/price-job',
	noun: 'price tracker',
	// The tracker's own fields: the model-data sync's are on its page, and a save here leaves them.
	paths: [
		'enabled',
		'schedule',
		'scan_schedule',
		'time_zone',
		'stale_days',
		'extraction_model',
		'extraction',
		'max_extractions_per_run',
		'scan'
	],
	// The extraction model is chosen among the models that can read a page into a schema.
	fields: async (event: KitEvent) => {
		const models = await allModels(event);
		const readers = models.filter(
			(m) =>
				(m.state === 'active' || m.state === 'deprecated') &&
				m.capabilities.structured_output === true
		);
		return {
			enabled: { label: 'Switched on' },
			schedule: { label: 'Check schedule', help: `When the confirmed pages are checked. ${CRON}` },
			scan_schedule: {
				label: 'Scan schedule',
				help: "When the providers' sites are scanned for pages that list models and prices, in the same form."
			},
			extraction_model: {
				help: "The model that reads the pages: an active chat model with structured output, called as the gateway's own workspace. Pages are not read until one is set.",
				options: readers.map((m) => ({ value: m.name, label: m.name, help: '' }))
			},
			extraction: {
				label: 'Template of earlier releases',
				help: 'The template in _system that read pricing pages before; used while no extraction model is set.'
			},
			max_extractions_per_run: {
				help: 'How many calls of the extraction model one run may make. A long page takes several.'
			},
			// The sources of earlier releases are price pages now; the bound is no longer used.
			sources: { hidden: true },
			pages_migrated: { hidden: true },
			auto_apply_bound: { hidden: true }
		};
	}
});

const importFields = () =>
	inputFields('/prices/import', 'post', {
		prices: {
			help: 'A JSON list, token rates per one million tokens: [{"model": "provider/model", "input": 3, "output": 15}].'
		}
	});

export async function load(event: KitEvent) {
	const [base, runs, proposals] = await Promise.all([
		job.load(event),
		event.locals.api.GET('/price-job/runs', { params: { query: { limit: 30 } } }),
		event.locals.api.GET('/price-proposals')
	]);
	return {
		...base,
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

	/** Asks for a check of every page now. It starts when the scraper next asks for work. */
	run: async (event: KitEvent) => {
		const result = await event.locals.api.POST('/price-job/run');
		if (!result.response.ok) return failed(event, 'run', result);
		return done(
			'run',
			'A check of every page was requested. It starts within a minute; its result appears in the list of runs.'
		);
	},

	/** Asks for a scan of the providers' sites now. */
	scan: async (event: KitEvent) => {
		const result = await event.locals.api.POST('/price-job/scan');
		if (!result.response.ok) return failed(event, 'scan', result);
		return done(
			'scan',
			'A scan was requested. The pages it finds wait under Pricing pages to be confirmed.'
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
