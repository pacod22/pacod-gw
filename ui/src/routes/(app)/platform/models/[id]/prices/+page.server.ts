import { error } from '@sveltejs/kit';
import type { ApiProblem } from '#lib/api/errors.ts';
import { readForm } from '#lib/schema/form.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	problemOf,
	type KitEvent
} from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

const priceFields = () =>
	inputFields('/models/{id}/price', 'put', {
		currency: {
			help: 'The currency the rates are in.',
			options: ['USD', 'EUR'].map((value) => ({ value, label: value, help: '' }))
		},
		input: { label: 'Input, per 1M tokens' },
		output: { label: 'Output, per 1M tokens' },
		cache_read: { label: 'Cache read, per 1M tokens' },
		cache_write_5m: { label: 'Cache write (5 minutes), per 1M tokens' },
		cache_write_1h: { label: 'Cache write (1 hour), per 1M tokens' },
		reasoning: { label: 'Reasoning, per 1M tokens' },
		variants: {
			help: 'Separate rates for variants the provider bills differently, as JSON: for example {"batch": {"input": 1.5, "output": 7.5}}.'
		},
		pinned: {
			label: 'Pin this price',
			help: "The price tracker and imports leave a pinned price alone. Pin a rate the provider's pages do not show, such as a negotiated one."
		}
	});

export const load: PageServerLoad = async (event) => {
	const path = { id: event.params.id };
	const [model, prices] = await Promise.all([
		event.locals.api.GET('/models/{id}', { params: { path } }),
		event.locals.api.GET('/models/{id}/prices', { params: { path } })
	]);
	if (model.response.status === 404) error(404, 'This model does not exist.');
	let problem: ApiProblem | null = null;
	if (!model.response.ok) problem = problemOf(event, model);
	else if (!prices.response.ok) problem = problemOf(event, prices);
	return {
		model: model.data
			? { id: model.data.id, name: model.data.name, free: model.data.free, state: model.data.state }
			: null,
		prices: prices.data ?? [],
		problem,
		fields: event.locals.principal?.platform_admin && !problem ? priceFields() : null
	};
};

export const actions = {
	/** Sets a new price. It becomes a new version; earlier versions stay for the calls they priced. */
	price: async (event: KitEvent) => {
		const form = await event.request.formData();
		const fields = priceFields();
		const result = await event.locals.api.PUT('/models/{id}/price', {
			params: { path: { id: event.params.id ?? '' } },
			body: readForm(fields, form) as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'price', result, { fields });
		return done(
			'price',
			`Price version ${result.data.version} was saved for “${result.data.model}”.`
		);
	},

	/** Pins or unpins the price in effect. It makes no new version. */
	pin: async (event: KitEvent) => {
		const pinned = formText(await event.request.formData(), 'pinned') === 'true';
		const result = await event.locals.api.POST('/models/{id}/price/pin', {
			params: { path: { id: event.params.id ?? '' } },
			body: { pinned }
		});
		if (!result.response.ok || !result.data) return failed(event, 'pin', result);
		return done(
			'pin',
			pinned
				? `The price of “${result.data.model}” is pinned: the price tracker and imports leave it alone.`
				: `The price of “${result.data.model}” is no longer pinned: the price tracker may replace it.`
		);
	}
};
