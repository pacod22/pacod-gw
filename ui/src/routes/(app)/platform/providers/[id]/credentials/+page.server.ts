import { error } from '@sveltejs/kit';
import { organisationOptions, workspaceOptions } from '#lib/server/options.ts';
import {
	done,
	failed,
	formText,
	problemOf,
	resource,
	type KitEvent
} from '#lib/server/resource.ts';

const credentials = resource({
	path: '/providers/{id}/credentials',
	noun: 'credential',
	nameOf: (credential) => credential.label || credential.fingerprint,
	// A scope names an organisation or a workspace by id: offer the ones that exist.
	fields: async (event) => {
		const [orgs, workspaces] = await Promise.all([
			organisationOptions(event),
			workspaceOptions(event)
		]);
		return {
			'scope.id@organisation': { label: 'Organisation', options: orgs },
			'scope.id@workspace': {
				label: 'Workspace',
				...(workspaces.length > 0 ? { options: workspaces } : {})
			}
		};
	},
	extra: async (event) => {
		const id = event.params.id ?? '';
		const result = await event.locals.api.GET('/providers/{id}', { params: { path: { id } } });
		if (!result.response.ok || !result.data) {
			const problem = problemOf(event, result);
			if (problem.status === 404) error(404, 'This provider does not exist.');
			return { provider: null };
		}
		return {
			provider: { id: result.data.id, name: result.data.name, auth: result.data.auth ?? null }
		};
	}
});

export const load = credentials.load;

export const actions = {
	...credentials.actions,

	/** Makes a credential the current one, or takes it out of use. */
	state: async (event: KitEvent) => {
		const form = await event.request.formData();
		const cid = formText(form, 'id');
		const state = formText(form, 'state') === 'current' ? 'current' : 'disabled';
		const result = await event.locals.api.POST('/providers/{id}/credentials/{cid}/state', {
			params: { path: { id: event.params.id ?? '', cid } },
			body: { state }
		});
		if (!result.response.ok || !result.data) return failed(event, 'state', result, { id: cid });
		const name = result.data.label || result.data.fingerprint;
		return done(
			'state',
			state === 'current'
				? `Credential “${name}” is now used for new requests.`
				: `Credential “${name}” is disabled.`,
			{ id: cid }
		);
	}
};
