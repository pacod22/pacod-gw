import { error } from '@sveltejs/kit';
import type { CopyTarget } from '#lib/credentials.ts';
import { readForm } from '#lib/schema/form.ts';
import { organisationOptions, scopeNames, workspaceOptions } from '#lib/server/options.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	problemOf,
	resource,
	type KitEvent
} from '#lib/server/resource.ts';

/** What a copy of a stored key asks for besides where it goes, which the page chooses. */
const copyFields = () =>
	inputFields('/providers/{id}/credentials/{cid}/copy', 'post', { scope: { hidden: true } });

/**
 * Where a stored key can be given: the organisations and workspaces, each with the
 * organisation it belongs to, since a key of an organisation goes to its own workspaces only.
 */
async function places(event: KitEvent): Promise<CopyTarget[]> {
	const names = await scopeNames(event);
	return [
		...names.organisations.map((o) => ({
			type: 'organisation' as const,
			id: o.id,
			organisation: o.id,
			label: `Organisation ${o.name}`
		})),
		...(names.workspaces ?? []).map((w) => ({
			type: 'workspace' as const,
			id: w.id,
			organisation: w.organisationId,
			label: `Workspace ${w.organisationName} / ${w.name}`
		}))
	];
}

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
		const [result, targets] = await Promise.all([
			event.locals.api.GET('/providers/{id}', { params: { path: { id } } }),
			places(event)
		]);
		if (!result.response.ok || !result.data) {
			const problem = problemOf(event, result);
			if (problem.status === 404) error(404, 'This provider does not exist.');
			return { provider: null, targets, copyFields: copyFields() };
		}
		return {
			provider: { id: result.data.id, name: result.data.name, auth: result.data.auth ?? null },
			targets,
			copyFields: copyFields()
		};
	}
});

export const load = credentials.load;

export const actions = {
	...credentials.actions,

	/** Gives an organisation or a workspace a copy of a stored key, as its own. */
	copy: async (event: KitEvent) => {
		const form = await event.request.formData();
		const cid = formText(form, 'id');
		const [type, id = ''] = formText(form, 'to').split(':');
		const scope =
			type === 'workspace'
				? { type: 'workspace' as const, id }
				: { type: 'organisation' as const, id };
		const fields = copyFields();
		const values = readForm(fields, form);
		const result = await event.locals.api.POST('/providers/{id}/credentials/{cid}/copy', {
			params: { path: { id: event.params.id ?? '', cid } },
			body: {
				scope,
				label: typeof values.label === 'string' ? values.label : null,
				make_current: values.make_current !== false,
				grace_hours: typeof values.grace_hours === 'number' ? values.grace_hours : null
			}
		});
		if (!result.response.ok || !result.data)
			return failed(event, 'copy', result, { id: cid, fields });
		const name = result.data.label || result.data.fingerprint;
		return done(
			'copy',
			result.data.state === 'current'
				? `The ${scope.type} now uses credential “${name}” as its own key.`
				: `The ${scope.type} has credential “${name}” as one of its keys, not yet in use.`,
			{ id: result.data.id }
		);
	},

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
