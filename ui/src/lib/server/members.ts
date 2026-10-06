/**
 * The members pages of an organisation and of a workspace: who has which role, the open
 * invitations, and the actions that change them. Organisation admins manage members; the
 * others see the list.
 */
import { error } from '@sveltejs/kit';
import { canAdminOrg } from '#lib/access.ts';
import type { ApiProblem } from '#lib/api/errors.ts';
import {
	ORG_ROLES,
	WORKSPACE_ROLES,
	type Invitation,
	type InviteResult,
	type Member,
	type RoleName,
	parseEmails
} from '#lib/members.ts';
import { done, failed, formText, problemOf, type KitEvent, type Outcome } from './resource';

export interface MembersData {
	scope: 'organisation' | 'workspace';
	members: Member[];
	invitations: Invitation[] | null;
	problem: ApiProblem | null;
	canManage: boolean;
	/** The organisation's workspaces, for the role pickers and the invitation form. */
	workspaces: { id: string; name: string }[];
}

function path(event: KitEvent) {
	const org = event.params.org;
	if (!org) error(404, 'Not found');
	return { org, ws: event.params.ws };
}

export async function loadMembers(event: KitEvent): Promise<MembersData> {
	const { org, ws } = path(event);
	const api = event.locals.api;
	const principal = event.locals.principal;
	const canManage = principal !== null && canAdminOrg(principal, org);

	const [members, invitations, workspaces] = await Promise.all([
		ws
			? api.GET('/orgs/{org}/workspaces/{ws}/members', { params: { path: { org, ws } } })
			: api.GET('/orgs/{org}/members', { params: { path: { org } } }),
		canManage
			? api.GET('/orgs/{org}/invitations', {
					params: { path: { org }, query: ws ? { workspace_id: ws } : {} }
				})
			: null,
		api.GET('/orgs/{org}/workspaces', { params: { path: { org }, query: { limit: 500 } } })
	]);
	let problem: ApiProblem | null = null;
	if (!members.response.ok) {
		problem = problemOf(event, members);
		if (problem.status === 404) error(404, problem.message);
	}
	return {
		scope: ws ? 'workspace' : 'organisation',
		members: members.data?.items ?? [],
		invitations: invitations?.data?.items ?? null,
		problem,
		canManage,
		workspaces: (workspaces.data?.items ?? []).map((w) => ({
			id: w.id,
			name: w.display_name || w.name
		}))
	};
}

function linkOf(event: KitEvent, token: string): string {
	return `${event.url.origin}/invite?token=${encodeURIComponent(token)}`;
}

/** A role that can be given in an organisation or a workspace; `null` for anything else. */
function asRole(value: string): RoleName | null {
	return [...ORG_ROLES, ...WORKSPACE_ROLES].find((r) => r === value) ?? null;
}

export const memberActions = {
	/** Invites one or more addresses to the organisation or one of its workspaces. */
	invite: async (event: KitEvent): Promise<Outcome> => {
		const { org } = path(event);
		const form = await event.request.formData();
		const emails = parseEmails(formText(form, 'emails'));
		const role = asRole(formText(form, 'role'));
		const where = formText(form, 'where');
		if (emails.length === 0 || !role) {
			return done('invite', 'Enter at least one email address and choose a role.', {
				tone: 'warning'
			});
		}
		const results: InviteResult[] = [];
		for (const email of emails) {
			const sent = await event.locals.api.POST('/orgs/{org}/invitations', {
				params: { path: { org } },
				body: { email, role, workspace_id: where || null }
			});
			if (!sent.response.ok || !sent.data) {
				results.push({
					email,
					ok: false,
					emailed: false,
					message: problemOf(event, sent).message
				});
				continue;
			}
			results.push({
				email,
				ok: true,
				emailed: sent.data.emailed,
				link: sent.data.token ? linkOf(event, sent.data.token) : undefined,
				message: sent.data.email_error ?? undefined
			});
		}
		const okCount = results.filter((r) => r.ok).length;
		const message =
			okCount === results.length
				? `${okCount === 1 ? 'Invitation' : `${okCount} invitations`} created.`
				: `${okCount} of ${results.length} invitations created.`;
		return done('invite', message, {
			tone: okCount === results.length ? 'success' : 'warning',
			data: { results }
		});
	},

	/**
	 * Sets a member's roles: one field per scope, `role:org` for the organisation and
	 * `role:<workspace id>` for a workspace, each a role or empty for none. Only the scopes
	 * whose value changed are written.
	 */
	access: async (event: KitEvent): Promise<Outcome> => {
		const { org } = path(event);
		const form = await event.request.formData();
		const principal = formText(form, 'principal');
		const name = formText(form, 'name') || 'the member';
		const api = event.locals.api;
		let changed = 0;
		for (const [key, value] of form.entries()) {
			if (typeof value !== 'string' || !key.startsWith('role:')) continue;
			const before = formText(form, `was:${key.slice(5)}`);
			if (value === before) continue;
			const target = key.slice(5);
			const role = value === '' ? null : asRole(value);
			const result =
				target === 'org'
					? await api.PUT('/orgs/{org}/members/{principal}', {
							params: { path: { org, principal } },
							body: { role }
						})
					: await api.PUT('/orgs/{org}/workspaces/{ws}/members/{principal}', {
							params: { path: { org, ws: target, principal } },
							body: { role }
						});
			if (!result.response.ok) return failed(event, 'access', result, { id: principal });
			changed += 1;
		}
		return done(
			'access',
			changed === 0 ? `Nothing changed for ${name}.` : `The access of ${name} was changed.`,
			{ id: principal }
		);
	},

	/** Takes away the member's roles: in the organisation and its workspaces, or in the workspace. */
	remove: async (event: KitEvent): Promise<Outcome> => {
		const { org, ws } = path(event);
		const form = await event.request.formData();
		const principal = formText(form, 'principal');
		const name = formText(form, 'name') || 'The member';
		const result = ws
			? await event.locals.api.DELETE('/orgs/{org}/workspaces/{ws}/members/{principal}', {
					params: { path: { org, ws, principal } }
				})
			: await event.locals.api.DELETE('/orgs/{org}/members/{principal}', {
					params: { path: { org, principal } }
				});
		if (!result.response.ok) return failed(event, 'remove', result, { id: principal });
		return done('remove', `${name} was removed${ws ? ' from the workspace' : ''}.`, {
			id: principal
		});
	},

	resend: async (event: KitEvent): Promise<Outcome> => {
		const { org } = path(event);
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const sent = await event.locals.api.POST('/orgs/{org}/invitations/{id}/resend', {
			params: { path: { org, id } }
		});
		if (!sent.response.ok || !sent.data) return failed(event, 'resend', sent, { id });
		const result: InviteResult = {
			email: sent.data.email,
			ok: true,
			emailed: sent.data.emailed,
			link: sent.data.token ? linkOf(event, sent.data.token) : undefined,
			message: sent.data.email_error ?? undefined
		};
		return done('resend', `A new invitation for ${sent.data.email} was created.`, {
			id,
			data: { results: [result] }
		});
	},

	revoke: async (event: KitEvent): Promise<Outcome> => {
		const { org } = path(event);
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.DELETE('/orgs/{org}/invitations/{id}', {
			params: { path: { org, id } }
		});
		if (!result.response.ok) return failed(event, 'revoke', result, { id });
		return done('revoke', `The invitation for ${formText(form, 'email')} was revoked.`, { id });
	}
};
