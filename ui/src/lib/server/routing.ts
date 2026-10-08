/** The routing-scheme pages: reading the editor's form, and loading schemes. */
import { fail, type ActionFailure } from '@sveltejs/kit';
import type { Schemas } from '#lib/api/types.ts';
import { planSchemes, type SchemeStep } from '#lib/routing.ts';
import { failed, type ActionOutcome, type KitEvent } from './resource.ts';

/** The scheme of the editor's hidden `scheme` field, or the answer to give when it is not JSON. */
export function schemeFromForm(
	form: FormData
): { body: unknown } | { failure: ActionFailure<ActionOutcome> } {
	const raw = form.get('scheme');
	try {
		return { body: JSON.parse(typeof raw === 'string' ? raw : '') };
	} catch (e) {
		return {
			failure: fail(400, {
				action: 'save',
				ok: false,
				problem: {
					status: 400,
					type: 'invalid_json',
					title: 'The scheme is not valid JSON',
					message: (e as Error).message,
					hint: 'Correct the JSON, or go back to the form.',
					placement: 'form',
					tone: 'error',
					reload: false,
					fields: {},
					lists: []
				}
			})
		};
	}
}

/** The routing schemes of an organisation, as choices of a form. */
export async function schemeOptions(
	event: KitEvent,
	org: string
): Promise<{ value: string; label: string; help: string }[]> {
	const list = await event.locals.api.GET('/orgs/{org}/routing-schemes', {
		params: { path: { org }, query: { limit: 500 } }
	});
	return (list.data?.items ?? []).map((s) => ({
		value: s.name,
		label: s.status === 'active' ? s.name : `${s.name} (disabled)`,
		help: s.description
	}));
}

/**
 * Creates the schemes of `steps` that the organisation lacks, in order. A 409 means the scheme
 * is there after all: created meanwhile, or the list came from an admin instance that had not
 * applied it yet. Answers the names created, or the failure to give.
 */
export async function createMissingSchemes(
	event: KitEvent,
	action: string,
	org: string,
	schemes: Schemas['RoutingScheme'][],
	steps: SchemeStep[]
): Promise<{ created: string[] } | { failure: ActionFailure<ActionOutcome> }> {
	const created: string[] = [];
	for (const step of steps.filter((s) => s.create)) {
		const body = schemes.find((s) => s.name === step.name);
		if (!body) continue;
		const result = await event.locals.api.POST('/orgs/{org}/routing-schemes', {
			params: { path: { org } },
			body
		});
		if (result.response.status === 409) continue;
		if (!result.response.ok) return { failure: failed(event, action, result) };
		created.push(step.name);
	}
	return { created };
}

/** The default routing schemes, and what loading them into the organisation would create. */
export async function defaultSchemes(event: KitEvent, org: string) {
	const api = event.locals.api;
	const [defaults, list] = await Promise.all([
		api.GET('/routing-scheme-defaults'),
		api.GET('/orgs/{org}/routing-schemes', { params: { path: { org }, query: { limit: 500 } } })
	]);
	const steps =
		defaults.data && list.data
			? planSchemes(
					defaults.data,
					list.data.items.map((s) => s.name)
				)
			: null;
	return { defaults, list, steps };
}
