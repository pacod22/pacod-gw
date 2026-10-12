/**
 * What a principal may do, as far as the console needs to know to hide what would be refused.
 * The core decides; both ask the same rules: the gateway's manifest (authz/gw.yaml) through
 * pacod-id's rules engine, whose vectors (authz/vectors/) run in the core's tests and here.
 */
import type { Binding, Subject } from '@pacod/id-authz';
import type { Principal } from './api/types';
import { catalog, type Permission } from './authz.generated';

export interface Scope {
	/** Selected organisation id. */
	org: string | null;
	/** Selected workspace id. */
	ws: string | null;
}

const orgPath = (org: string) => ['platform', `organisation:${org}`];
const workspacePath = (ws: string, org: string) => [...orgPath(org), `gw.workspace:${ws}`];

/**
 * The principal's roles as bindings of the rules engine. A workspace role needs its
 * organisation, which the core sends with it; one without is left out and grants nothing.
 */
export function subjectOf(p: Principal): Subject {
	const bindings: Binding[] = [];
	for (const r of p.roles) {
		const role = `gw.${r.role}`;
		if (r.scope.type === 'platform') bindings.push({ role, path: ['platform'] });
		else if (r.scope.type === 'organisation') bindings.push({ role, path: orgPath(r.scope.id) });
		else if (r.organisation_id)
			bindings.push({ role, path: workspacePath(r.scope.id, r.organisation_id) });
	}
	return { kind: p.kind === 'user' ? 'user' : 'service', bindings };
}

const allowed = (p: Principal, permission: Permission, at: string[]) =>
	catalog.allowed(subjectOf(p), permission, at);

/** Any role that applies to the whole installation. */
export function hasPlatformRole(p: Principal): boolean {
	return catalog.holdsAnyAt(subjectOf(p), 'platform');
}

/** Any role on the platform or on this organisation; a workspace role is not enough. */
export function canReadOrg(p: Principal, org: string): boolean {
	return allowed(p, 'gw.org.read', orgPath(org));
}

export function canAdminOrg(p: Principal, org: string): boolean {
	return allowed(p, 'gw.org.manage', orgPath(org));
}

export function canAdminWorkspace(p: Principal, ws: string, org: string): boolean {
	return allowed(p, 'gw.workspace.manage', workspacePath(ws, org));
}

/**
 * The audit log of the installation (no `org`): platform admins and platform auditors. Of an
 * organisation: also its admins and auditors.
 */
export function canReadAuditLog(p: Principal, org?: string): boolean {
	return allowed(p, 'gw.audit_log.read', org === undefined ? ['platform'] : orgPath(org));
}

/** Whether the principal may write to an API path, judged by the scope the path lies in. */
export function canWrite(
	p: Principal,
	scope: 'platform' | 'organisation' | 'workspace',
	params: Record<string, string | undefined>
): boolean {
	if (scope === 'workspace') return canAdminWorkspace(p, params.ws ?? '', params.org ?? '');
	if (scope === 'organisation') return canAdminOrg(p, params.org ?? '');
	return p.platform_admin;
}

const ROLE_LABELS: Record<string, string> = {
	platform_admin: 'Platform admin',
	org_admin: 'Organisation admin',
	workspace_admin: 'Workspace admin',
	auditor: 'Auditor',
	viewer: 'Viewer'
};

/** "Organisation admin", "Viewer": how the console names a role. */
export function roleLabel(role: string): string {
	return ROLE_LABELS[role] ?? role.replace(/_/g, ' ');
}

/** "Platform admin", "Viewer of workspace Billing (Acme)": one line per role. */
export function describeRole(role: Principal['roles'][number]): string {
	const label = roleLabel(role.role);
	if (role.scope.type === 'platform')
		return role.role === 'platform_admin' ? label : `${label} of the platform`;
	const name = role.scope_name || role.scope.id;
	if (role.scope.type === 'organisation') return `${label} of organisation ${name}`;
	const org = role.organisation_name ? ` (${role.organisation_name})` : '';
	return `${label} of workspace ${name}${org}`;
}

/**
 * Audit records hold prompts and answers: for administrators and auditors, never for
 * viewers. True when the principal has such a role anywhere; the core decides per workspace.
 */
export function canReadAuditRecords(p: Principal): boolean {
	return catalog.allowedSomewhere(subjectOf(p), 'gw.audit_records.read');
}
