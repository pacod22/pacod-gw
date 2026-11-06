import { error } from '@sveltejs/kit';
import type { Option } from '#lib/schema/form.ts';
import { readForm } from '#lib/schema/form.ts';
import { providerOptions } from '#lib/server/options.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	problemOf,
	type KitEvent
} from '#lib/server/resource.ts';

const pageFields = (providers: Option[]) =>
	inputFields('/price-pages', 'post', {
		provider_id: { label: 'Provider', options: providers },
		url: {
			label: 'Address',
			help: "A pricing or model page of the provider's, or a machine-readable price list."
		},
		kind: {
			help: 'page: rendered in the browser and read by the extraction model. feed: a price list, read as it is.'
		},
		format: { help: "For a feed: litellm (LiteLLM's list) or pacod (the gateway's own format)." },
		key_prefix: {
			help: "For a feed: the prefix the list files the provider's models under, where it is not the provider's name (LiteLLM: zai for GLM, moonshot for Kimi, gemini, deepseek)."
		},
		priority: {
			help: 'Where two pages of the provider say different things about a model, the lower number wins.'
		}
	});

export async function load(event: KitEvent) {
	if (!event.locals.principal?.platform_admin)
		error(403, "The price tracker's pages are for platform admins.");
	const [pages, providers] = await Promise.all([
		event.locals.api.GET('/price-pages'),
		providerOptions(event)
	]);
	return {
		pages: pages.data ?? [],
		problem: pages.response.ok ? null : problemOf(event, pages),
		fields: pageFields(providers)
	};
}

/** A decision on one page: confirm, dismiss or check it. */
async function decide(event: KitEvent, what: 'confirm' | 'dismiss' | 'check') {
	const form = await event.request.formData();
	const id = formText(form, 'id');
	const url = formText(form, 'url');
	const result = await event.locals.api.POST(`/price-pages/{id}/${what}`, {
		params: { path: { id } }
	});
	if (!result.response.ok) return failed(event, what, result, { id });
	const said = {
		confirm: `${url} is read from the next check on, which starts within a minute.`,
		dismiss: `${url} is not read, and the scan does not propose it again.`,
		check: `${url} is read at the next check, which starts within a minute.`
	};
	return done(what, said[what], { id });
}

export const actions = {
	/** Adds a page of a provider's. It is read from the next check on. */
	add: async (event: KitEvent) => {
		const form = await event.request.formData();
		const fields = pageFields(await providerOptions(event));
		const result = await event.locals.api.POST('/price-pages', {
			body: readForm(fields, form) as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'add', result, { fields });
		return done('add', `${result.data.url} was added and is read at the next check.`);
	},
	confirm: (event: KitEvent) => decide(event, 'confirm'),
	dismiss: (event: KitEvent) => decide(event, 'dismiss'),
	check: (event: KitEvent) => decide(event, 'check'),

	/** Removes a page: it is no longer read, and the scan may propose it again. */
	remove: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.DELETE('/price-pages/{id}', { params: { path: { id } } });
		if (!result.response.ok) return failed(event, 'remove', result, { id });
		return done('remove', `${formText(form, 'url')} was removed.`);
	}
};
