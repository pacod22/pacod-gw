/**
 * An organisation's or a workspace's own provider keys, on its settings page. The API keeps
 * each key under its provider (`providers/{id}/credentials`) with a scope; this gathers the
 * keys of one scope across the providers.
 */
import { canAdminOrg, canAdminWorkspace } from '#lib/access.ts';
import type { Credential } from '#lib/api/types.ts';
import { readForm, type Field, type Option } from '#lib/schema/form.ts';
import { providerOptions } from './options.ts';
import { done, failed, formText, inputFields, type KitEvent } from './resource.ts';

export type OwnScope = { type: 'organisation'; id: string } | { type: 'workspace'; id: string };

export type OwnCredential = Credential & { provider_name: string };

/** The form for a new key: its provider, then what the API asks for, the scope set by the page. */
function addFields(providers: Option[]): Field[] {
	const provider: Field = {
		kind: 'enum',
		key: 'provider_id',
		path: 'provider_id',
		label: 'Provider',
		help: 'The provider whose account the key belongs to.',
		required: true,
		nullable: false,
		options: providers
	};
	return [
		provider,
		...inputFields('/providers/{id}/credentials', 'post', { scope: { hidden: true } })
	];
}

function mayManage(event: KitEvent, scope: OwnScope): boolean {
	const p = event.locals.principal;
	if (!p) return false;
	return scope.type === 'organisation'
		? canAdminOrg(p, scope.id)
		: canAdminWorkspace(p, scope.id, event.params.org ?? '');
}

/** The scope's keys, the form for a new one, and whether the caller may manage them. */
export async function loadOwnCredentials(event: KitEvent, scope: OwnScope) {
	if (!mayManage(event, scope)) {
		return {
			credentials: [] as OwnCredential[],
			credentialFields: [] as Field[],
			mayManageCredentials: false
		};
	}
	const providers = await providerOptions(event);
	const lists = await Promise.all(
		providers.map((p) =>
			event.locals.api.GET('/providers/{id}/credentials', { params: { path: { id: p.value } } })
		)
	);
	const credentials: OwnCredential[] = lists.flatMap((result, i) =>
		(result.data?.items ?? [])
			.filter((c) => c.scope.type === scope.type && 'id' in c.scope && c.scope.id === scope.id)
			.map((c) => ({ ...c, provider_name: providers[i].label }))
	);
	return { credentials, credentialFields: addFields(providers), mayManageCredentials: true };
}

const nameOf = (c: { label: string; fingerprint: string }) => c.label || c.fingerprint;

/** Adding a key, making one current or disabling it, and deleting one. */
export function credentialActions(scopeOf: (event: KitEvent) => OwnScope) {
	return {
		addCredential: async (event: KitEvent) => {
			const form = await event.request.formData();
			const fields = addFields([]);
			const values = readForm(fields, form);
			const result = await event.locals.api.POST('/providers/{id}/credentials', {
				params: { path: { id: formText(form, 'provider_id') } },
				body: {
					label: typeof values.label === 'string' ? values.label : '',
					secret: typeof values.secret === 'string' ? values.secret : '',
					make_current: values.make_current !== false,
					grace_hours: typeof values.grace_hours === 'number' ? values.grace_hours : null,
					scope: scopeOf(event)
				}
			});
			if (!result.response.ok || !result.data) {
				return failed(event, 'addCredential', result, { fields });
			}
			return done(
				'addCredential',
				result.data.state === 'current'
					? `Key “${nameOf(result.data)}” added and used for new requests.`
					: `Key “${nameOf(result.data)}” added.`,
				{ id: result.data.id }
			);
		},

		credentialState: async (event: KitEvent) => {
			const form = await event.request.formData();
			const cid = formText(form, 'id');
			const state = formText(form, 'state') === 'current' ? 'current' : 'disabled';
			const result = await event.locals.api.POST('/providers/{id}/credentials/{cid}/state', {
				params: { path: { id: formText(form, 'provider_id'), cid } },
				body: { state }
			});
			if (!result.response.ok || !result.data) {
				return failed(event, 'credentialState', result, { id: cid });
			}
			return done(
				'credentialState',
				state === 'current'
					? `Key “${nameOf(result.data)}” is now used for new requests.`
					: `Key “${nameOf(result.data)}” is disabled.`,
				{ id: cid }
			);
		},

		deleteCredential: async (event: KitEvent) => {
			const form = await event.request.formData();
			const cid = formText(form, 'id');
			const result = await event.locals.api.DELETE('/providers/{id}/credentials/{cid}', {
				params: { path: { id: formText(form, 'provider_id'), cid } }
			});
			if (!result.response.ok) return failed(event, 'deleteCredential', result, { id: cid });
			return done('deleteCredential', 'The key is deleted.', { id: cid });
		}
	};
}
