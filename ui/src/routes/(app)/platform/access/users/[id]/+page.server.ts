/**
 * One user: the account, its roles and everything that can be done with it, on one page.
 * `new` as the id creates a user.
 */
import { error, redirect } from '@sveltejs/kit';
import type { ApiProblem } from '#lib/api/errors.ts';
import type { User } from '#lib/api/types.ts';
import { buildFields, readForm, type FormMode } from '#lib/schema/form.ts';
import { describeResource } from '#lib/schema/spec.ts';
import { done, failed, formText, problemOf, type KitEvent } from '#lib/server/resource.ts';
import { applyRoles, loadRoles, rolesFailed } from '#lib/server/roles.ts';
import { spec } from '#lib/server/spec.ts';

const shape = describeResource(spec, '/users');
const LIST = '/platform/access/users';

function fieldsFor(mode: FormMode) {
	const input = mode === 'create' ? shape.create?.input : shape.replace?.input;
	if (!input) error(405, 'Users cannot be changed through the API.');
	return buildFields(spec, input, {
		mode,
		overrides: {
			version: { hidden: true },
			// The schema cannot say "required when creating": the description does.
			password: {
				required: 'create',
				help:
					mode === 'create'
						? 'At least 12 characters. The user can change it after signing in.'
						: 'A new password for the user, at least 12 characters.'
			}
		}
	});
}

const userPath = (id: string) => `${LIST}/${encodeURIComponent(id)}`;

export const load = async (event: KitEvent) => {
	const id = event.params.id ?? '';
	const isNew = id === 'new';
	const [result, roles] = await Promise.all([
		isNew ? null : event.locals.api.GET('/users/{id}', { params: { path: { id } } }),
		loadRoles(event)
	]);
	let user: User | null = null;
	let problem: ApiProblem | null = null;
	if (result) {
		if (result.response.ok && result.data) {
			user = result.data;
		} else {
			problem = problemOf(event, result);
			if (problem.status === 404) error(404, problem.message);
		}
	}
	return {
		isNew,
		user,
		problem,
		fields: fieldsFor(isNew ? 'create' : 'replace'),
		roles: roles.roles ? (roles.roles[id] ?? []) : null,
		rolesProblem: roles.rolesProblem,
		scopes: roles.scopes,
		canManage: roles.canManageRoles
	};
};

/** The id of the page, for the actions that work on an existing user. */
function existing(event: KitEvent): string {
	const id = event.params.id;
	if (!id || id === 'new') error(404, 'Not found');
	return id;
}

export const actions = {
	/** Creates the user or saves the account, then gives and takes away roles. */
	save: async (event: KitEvent) => {
		const id = event.params.id ?? '';
		const isNew = id === 'new';
		const form = await event.request.formData();
		const fields = fieldsFor(isNew ? 'create' : 'replace');
		const body: Record<string, unknown> = readForm(fields, form);
		const api = event.locals.api;

		if (isNew) {
			const created = await api.POST('/users', { body: body as never });
			if (!created.response.ok || !created.data) {
				return failed(event, 'save', created, { fields });
			}
			const roles = await applyRoles(event, created.data.id, form);
			// The user exists now: whatever happened to the roles is said on the user's page.
			const query = new URLSearchParams({ created: '1' });
			const trouble =
				roles.invalid ?? (roles.failure ? problemOf(event, roles.failure).message : '');
			if (trouble) query.set('roles_failed', trouble);
			redirect(303, `${userPath(created.data.id)}?${query}`);
		}

		const version = Number(formText(form, 'version'));
		if (formText(form, 'version') !== '' && Number.isInteger(version)) body.version = version;
		const saved = await api.PUT('/users/{id}', {
			params: { path: { id } },
			body: body as never
		});
		if (!saved.response.ok) return failed(event, 'save', saved, { id, fields });
		const roles = await applyRoles(event, id, form);
		if (roles.invalid) return done('save', roles.invalid, { tone: 'warning', id });
		if (roles.failure) {
			return rolesFailed(event, 'save', roles.failure, ['saved the account', ...roles.changes], id);
		}
		return done('save', `Your changes to ${formText(form, 'email') || 'the user'} were saved.`, {
			id
		});
	},

	/** Emails the user a new link to confirm the address. */
	verification: async (event: KitEvent) => {
		const id = existing(event);
		const form = await event.request.formData();
		const result = await event.locals.api.POST('/users/{id}/verification', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'verification', result, { id });
		return done('verification', `A verification link was sent to ${formText(form, 'email')}.`, {
			id
		});
	},

	/** Emails the user a link to choose a new password. */
	reset: async (event: KitEvent) => {
		const id = existing(event);
		const form = await event.request.formData();
		const result = await event.locals.api.POST('/users/{id}/password-reset', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'reset', result, { id });
		return done('reset', `A password-reset link was sent to ${formText(form, 'email')}.`, { id });
	},

	/** Marks the address as confirmed without an email, for example when sending fails. */
	confirm: async (event: KitEvent) => {
		const id = existing(event);
		const form = await event.request.formData();
		const result = await event.locals.api.POST('/users/{id}/email-confirmation', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'confirm', result, { id });
		return done('confirm', `The address ${formText(form, 'email')} is now confirmed.`, { id });
	},

	delete: async (event: KitEvent) => {
		const id = existing(event);
		const form = await event.request.formData();
		const result = await event.locals.api.DELETE('/users/{id}', { params: { path: { id } } });
		if (!result.response.ok) return failed(event, 'delete', result, { id });
		redirect(303, `${LIST}?${new URLSearchParams({ deleted: formText(form, 'email') })}`);
	}
};
