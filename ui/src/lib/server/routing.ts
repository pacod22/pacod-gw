/** The routing-scheme pages: reading the editor's form. */
import { fail, type ActionFailure } from '@sveltejs/kit';
import type { ActionOutcome, KitEvent } from './resource.ts';

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
