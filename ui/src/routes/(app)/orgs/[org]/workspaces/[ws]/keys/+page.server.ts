import { readForm } from '#lib/schema/form.ts';
import { grantedModels, modelOptions, templateOptions } from '#lib/server/options.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	resource,
	type KitEvent
} from '#lib/server/resource.ts';

const keys = resource({
	path: '/orgs/{org}/workspaces/{ws}/keys',
	noun: 'API key',
	// The allow-lists name models and templates: offer the ones this workspace has.
	fields: async (event) => {
		const org = event.params.org ?? '';
		const ws = event.params.ws ?? '';
		const [models, templates] = await Promise.all([
			grantedModels(event, org, ws),
			templateOptions(event, org, ws)
		]);
		return {
			allowed_models: {
				help: 'The models this key may use. None chosen means every model the workspace may use.',
				options: modelOptions(models, 'name')
			},
			allowed_templates: {
				help: 'The templates this key may use. None chosen means every template of the workspace.',
				options: templates
			}
		};
	},
	// The form of the rotation dialog comes from the schema of its request body.
	extra: async () => ({
		rotateFields: inputFields('/orgs/{org}/workspaces/{ws}/keys/{id}/rotate')
	})
});

function keyPath(event: KitEvent, id: string) {
	return { org: event.params.org ?? '', ws: event.params.ws ?? '', id };
}

export const load = keys.load;

export const actions = {
	...keys.actions,

	/** Gives the key a new secret. The previous one keeps working for the grace period. */
	rotate: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const fields = inputFields('/orgs/{org}/workspaces/{ws}/keys/{id}/rotate');
		const grace = readForm(fields, form).grace_hours;
		const result = await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/keys/{id}/rotate', {
			params: { path: keyPath(event, id) },
			// Left empty, the core's default grace period applies.
			body: { grace_hours: typeof grace === 'number' ? grace : null }
		});
		if (!result.response.ok || !result.data) return failed(event, 'rotate', result, { id, fields });
		const until = result.data.previous_secret_valid_until;
		return done('rotate', `API key “${result.data.name}” has a new secret.`, {
			id,
			secret: {
				title: `New secret of API key “${result.data.name}”`,
				value: result.data.secret,
				note: until
					? `This is the only time the new secret is shown. The previous secret keeps working until ${until.slice(0, 16).replace('T', ' ')} UTC: replace it in the calling service before then.`
					: 'This is the only time the new secret is shown. The previous secret stopped working: replace it in the calling service now.'
			}
		});
	},

	/** Revokes the key. It cannot be undone. */
	revoke: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/keys/{id}/revoke', {
			params: { path: keyPath(event, id) }
		});
		if (!result.response.ok || !result.data) return failed(event, 'revoke', result, { id });
		return done('revoke', `API key “${result.data.name}” was revoked.`, { id });
	}
};
