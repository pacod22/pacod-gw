/**
 * The roles of users and admin tokens: who has which role, and the changes the role editor
 * asks for. Role bindings are the underlying resource.
 */
import type { ApiProblem } from '#lib/api/errors.ts';
import { roleLabel } from '#lib/access.ts';
import type { RoleBinding, RoleName, RoleScope, RoleView } from '#lib/api/types.ts';
import { rolesFor } from '#lib/members.ts';
import type { ApiResult } from './api';
import { scopeNames, type ScopeNames } from './options';
import { done, failed, formText, problemOf, type KitEvent, type Outcome } from './resource';

/** A role someone has, with the binding that gives it. */
export type HeldRole = RoleView & { binding_id: string };

export type RolesData = {
	/** By principal id; `null` when the role bindings could not be read. */
	roles: Record<string, HeldRole[]> | null;
	rolesProblem: ApiProblem | null;
	/** Where a role can be given. */
	scopes: ScopeNames;
	canManageRoles: boolean;
};

const PAGE = 500;
/** Stops reading at 50 000 bindings rather than looping without end. */
const MAX_PAGES = 100;

const ORDER = { platform: 0, organisation: 1, workspace: 2 };

/** One page of role bindings, after `cursor`. */
function bindingsAfter(event: KitEvent, cursor: string | null) {
	return event.locals.api.GET('/role-bindings', {
		params: { query: { limit: PAGE, ...(cursor ? { cursor } : {}) } }
	});
}

export async function loadRoles(event: KitEvent): Promise<RolesData> {
	const scopes = scopeNames(event);
	const bindings: RoleBinding[] = [];
	let rolesProblem: ApiProblem | null = null;
	let cursor: string | null = null;
	for (let page = 0; page < MAX_PAGES; page++) {
		const result = await bindingsAfter(event, cursor);
		if (!result.response.ok || !result.data) {
			rolesProblem = problemOf(event, result);
			break;
		}
		bindings.push(...result.data.items);
		cursor = result.data.next_cursor ?? null;
		if (!cursor) break;
	}

	const names = await scopes;
	const orgName = new Map(names.organisations.map((o) => [o.id, o.name]));
	const workspace = new Map((names.workspaces ?? []).map((w) => [w.id, w]));
	const roles: Record<string, HeldRole[]> = {};
	for (const b of bindings) {
		const ws = b.scope.type === 'workspace' ? workspace.get(b.scope.id) : undefined;
		const held: HeldRole = {
			binding_id: b.id,
			role: b.role,
			scope: b.scope,
			scope_name:
				b.scope.type === 'organisation' ? (orgName.get(b.scope.id) ?? null) : (ws?.name ?? null),
			organisation_name: ws?.organisationName ?? null
		};
		(roles[b.principal] ??= []).push(held);
	}
	for (const list of Object.values(roles)) {
		list.sort(
			(a, b) =>
				ORDER[a.scope.type] - ORDER[b.scope.type] ||
				(a.scope_name ?? '').localeCompare(b.scope_name ?? '')
		);
	}
	return {
		roles: rolesProblem ? null : roles,
		rolesProblem,
		scopes: names,
		canManageRoles: event.locals.principal?.platform_admin === true
	};
}

/** `platform`, `organisation:<id>` or `workspace:<id>`, as the role editor sends it. */
function scopeOf(place: string, typedWorkspace = ''): RoleScope | null {
	if (place === 'platform') return { type: 'platform' };
	const org = place.match(/^organisation:(.+)$/);
	if (org) return { type: 'organisation', id: org[1] };
	const ws = place.match(/^workspace:(.*)$/);
	const id = ws ? ws[1] || typedWorkspace.trim() : '';
	return id ? { type: 'workspace', id } : null;
}

interface NewRole {
	role: string;
	scope: RoleScope | null;
	label: string;
}

/**
 * The roles the role editor asks for: each queued one as `add` (`role|place|label`), and the
 * one still chosen in its pickers.
 */
function newRoles(form: FormData): NewRole[] {
	const out: NewRole[] = [];
	for (const value of form.getAll('add')) {
		if (typeof value !== 'string') continue;
		const [role = '', place = '', ...label] = value.split('|');
		out.push({ role, scope: scopeOf(place), label: label.join('|') });
	}
	const role = formText(form, 'role');
	if (role) {
		out.push({
			role,
			scope: scopeOf(formText(form, 'place'), formText(form, 'workspace')),
			label: formText(form, 'added')
		});
	}
	return out;
}

export interface RoleChanges {
	/** What was done, in words: "added Viewer of the platform". */
	changes: string[];
	/** The call that failed, when one did: the changes before it stay done. */
	failure: ApiResult | null;
	/** A request that cannot be sent, such as a role that does not fit its place. */
	invalid: string | null;
}

/**
 * Gives the new roles of a role editor's form and takes away those whose box was cleared.
 * New roles go first, so that a change that fails halfway does not leave someone without
 * access. Stops at the first failure.
 */
export async function applyRoles(
	event: KitEvent,
	principal: string,
	form: FormData
): Promise<RoleChanges> {
	const api = event.locals.api;
	const changes: string[] = [];
	const additions = newRoles(form);
	for (const added of additions) {
		const fits = added.scope ? rolesFor(added.scope.type).find((r) => r === added.role) : undefined;
		if (!added.scope || !fits) {
			return {
				changes,
				failure: null,
				invalid: 'Choose where each new role applies and a role that fits there.'
			};
		}
	}
	for (const added of additions) {
		const created = await api.POST('/role-bindings', {
			body: { principal, role: added.role as RoleName, scope: added.scope as RoleScope }
		});
		if (!created.response.ok) return { changes, failure: created, invalid: null };
		changes.push(`added ${added.label || roleLabel(added.role)}`);
	}

	const keep = new Set(form.getAll('keep'));
	for (const id of form.getAll('held')) {
		if (typeof id !== 'string' || keep.has(id)) continue;
		const removed = await api.DELETE('/role-bindings/{id}', { params: { path: { id } } });
		if (!removed.response.ok) return { changes, failure: removed, invalid: null };
		changes.push(`removed ${formText(form, `label:${id}`) || 'a role'}`);
	}
	return { changes, failure: null, invalid: null };
}

/** The failure of an action that changed roles: what went wrong, and what was done before. */
export function rolesFailed(
	event: KitEvent,
	action: string,
	result: ApiResult,
	done: string[],
	id: string
) {
	const failure = failed(event, action, result, { id });
	if (done.length > 0) failure.data.problem?.lists.push({ label: 'Already done', items: done });
	return failure;
}

export const roleActions = {
	/** Changes the roles of the user or admin token named by `principal`. */
	roles: async (event: KitEvent): Promise<Outcome> => {
		const form = await event.request.formData();
		const principal = formText(form, 'principal');
		const name = formText(form, 'name') || 'them';
		const result = await applyRoles(event, principal, form);
		if (result.invalid) return done('roles', result.invalid, { tone: 'warning', id: principal });
		if (result.failure) {
			return rolesFailed(event, 'roles', result.failure, result.changes, principal);
		}
		return done(
			'roles',
			result.changes.length === 0
				? `Nothing changed for ${name}.`
				: `The roles of ${name} were changed.`,
			{ id: principal }
		);
	}
};
