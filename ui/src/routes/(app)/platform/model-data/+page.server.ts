import { fail } from '@sveltejs/kit';
import { localProblem } from '#lib/api/errors.ts';
import { done, failed, objectResource, problemOf, type KitEvent } from '#lib/server/resource.ts';

const CRON =
	'A cron expression with five fields: minute, hour, day of month, month, day of week. Days of the week run from 0 (Sunday) to 7 (Sunday again) or by name (mon-fri). Restrict the day of the month or the day of the week, not both.';

const job = objectResource({
	path: '/price-job',
	noun: 'model-data sync',
	paths: [
		'source',
		'enabled',
		'schedule',
		'time_zone',
		'stale_days',
		'apply_properties',
		'openrouter_url',
		'openrouter_price_vendors',
		'litellm_url'
	],
	fields: {
		source: {
			label: 'Source',
			help: "Where model information and prices come from each day: pacod-aggregator (what the vendors' own pages say), OpenRouter's list, LiteLLM's list, or the models file of the configuration alone."
		},
		enabled: { label: 'Switched on' },
		schedule: { label: 'Schedule', help: `When the model data is synced. ${CRON}` },
		time_zone: { label: 'Time zone', help: 'The IANA time zone the schedule is read in.' },
		stale_days: {
			label: 'Stale after (days)',
			help: 'A price that was neither set nor confirmed by a source for longer raises an alert.'
		},
		apply_properties: {
			label: 'Apply properties',
			help: 'Context windows, output limits, modalities and capabilities the source states apply too, where the source changed them. An edit of yours stands until the source changes the value.'
		},
		openrouter_url: { label: "OpenRouter's list" },
		openrouter_price_vendors: {
			label: 'Vendors whose OpenRouter prices apply',
			help: "For other vendors, open-weight models in particular, OpenRouter's price is usually a third party's. Vendor ids: openai, anthropic, google, xai, deepseek, zai, moonshot, mistral, …"
		},
		litellm_url: { label: "LiteLLM's list" }
	}
});

export async function load(event: KitEvent) {
	const [base, runs] = await Promise.all([
		job.load(event),
		event.locals.api.GET('/price-job/runs', { params: { query: { job: 'sync', limit: 30 } } })
	]);
	return {
		...base,
		runs: runs.data ?? [],
		runsProblem: runs.response.ok ? null : problemOf(event, runs)
	};
}

const FORMATS = ['auto', 'catalog', 'openrouter', 'litellm', 'pacod'];

export const actions = {
	...job.actions,

	/** Asks for a sync now. It starts at the next round of upkeep, within a minute. */
	run: async (event: KitEvent) => {
		const result = await event.locals.api.POST('/price-job/run');
		if (!result.response.ok) return failed(event, 'run', result);
		return done(
			'run',
			'A sync of the model data was requested. It starts within a minute; its result appears in the list of runs.'
		);
	},

	/** Applies an uploaded file of models, parameters and prices. */
	upload: async (event: KitEvent) => {
		const form = await event.request.formData();
		const file = form.get('file');
		const format = String(form.get('format') ?? 'auto');
		if (!(file instanceof File) || file.size === 0) {
			return fail(400, { action: 'upload', ok: false, problem: localProblem('Choose a file.') });
		}
		if (!FORMATS.includes(format)) {
			return fail(400, { action: 'upload', ok: false, problem: localProblem('Choose a format.') });
		}
		let body: unknown;
		try {
			body = JSON.parse(await file.text());
		} catch {
			return fail(400, {
				action: 'upload',
				ok: false,
				problem: localProblem(`${file.name} is not a JSON file.`)
			});
		}
		const result = await event.locals.api.POST('/price-job/upload', {
			params: { query: { format } },
			body: body as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'upload', result);
		const counts = Object.entries((result.data.counts ?? {}) as Record<string, unknown>)
			.map(([outcome, count]) => `${count} ${outcome.replaceAll('_', ' ')}`)
			.join(', ');
		return done('upload', `${file.name} was applied${counts ? `: ${counts}` : ''}.`, {
			data: result.data.models
		});
	}
};
