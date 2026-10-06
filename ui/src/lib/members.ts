/**
 * Members of organisations and workspaces: the roles that can be given and how they are
 * described. Shared by the members pages and their server side.
 */
import { roleLabel } from './access';
import type { Schemas } from './api/types';

export type Member = Schemas['Member'];
export type Invitation = Schemas['InvitationView'];
export type RoleName = Schemas['RoleName'];

/** The roles that can be given in each kind of scope, most powerful first. */
export const PLATFORM_ROLES: RoleName[] = ['platform_admin', 'auditor', 'viewer'];
export const ORG_ROLES: RoleName[] = ['org_admin', 'auditor', 'viewer'];
export const WORKSPACE_ROLES: RoleName[] = ['workspace_admin', 'auditor', 'viewer'];

/** The roles that fit a kind of scope, as the core allows them. */
export function rolesFor(type: 'platform' | 'organisation' | 'workspace'): RoleName[] {
	if (type === 'platform') return PLATFORM_ROLES;
	return type === 'organisation' ? ORG_ROLES : WORKSPACE_ROLES;
}

/** What each role may do, in a line, for the role pickers. */
export const ROLE_HELP: Record<string, string> = {
	platform_admin: 'Runs the whole installation.',
	org_admin: 'Runs the organisation: its workspaces, members, settings and limits.',
	workspace_admin: 'Runs the workspace: API keys, templates, limits and settings.',
	auditor: 'Reads configuration, usage, audit records and the audit log; changes nothing.',
	viewer: 'Reads configuration and usage; no secrets, no audit records.'
};

export interface InviteResult {
	email: string;
	ok: boolean;
	/** Sent by email. */
	emailed: boolean;
	/** The link to hand on, when it was not emailed. */
	link?: string;
	/** Why it failed, or why the email could not be sent. */
	message?: string;
}

/** "Workspace admin of Billing": a member's role in words, for lists. */
export function describeMemberRole(role: Member['roles'][number]): string {
	const label = roleLabel(role.role);
	if (role.scope.type === 'organisation') return label;
	return `${label} of ${role.scope_name ?? ('id' in role.scope ? role.scope.id : '')}`;
}

/** "a@x, b@y" or one per line: the addresses, deduplicated. */
export function parseEmails(text: string): string[] {
	const all = text
		.split(/[\s,;]+/)
		.map((e) => e.trim().toLowerCase())
		.filter(Boolean);
	return [...new Set(all)];
}
