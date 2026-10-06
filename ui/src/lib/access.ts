/**
 * What a principal may do, as far as the console needs to know to hide what would be refused.
 * The core decides; these rules mirror its role model (concept section 5.4).
 */
import type { Principal } from './api/types';

export interface Scope {
	/** Selected organisation id. */
	org: string | null;
	/** Selected workspace id. */
	ws: string | null;
}

function has(p: Principal, role: string, type: string, id?: string): boolean {
	return p.roles.some(
		(r) =>
			r.role === role &&
			r.scope.type === type &&
			(id === undefined || ('id' in r.scope && r.scope.id === id))
	);
}

/** Any role that applies to the whole installation. */
export function hasPlatformRole(p: Principal): boolean {
	return p.platform_admin || p.roles.some((r) => r.scope.type === 'platform');
}

/** Any role on the platform or on this organisation; a workspace role is not enough. */
export function canReadOrg(p: Principal, org: string): boolean {
	return (
		p.platform_admin ||
		p.roles.some(
			(r) => r.scope.type === 'platform' || (r.scope.type === 'organisation' && r.scope.id === org)
		)
	);
}

export function canAdminOrg(p: Principal, org: string): boolean {
	return p.platform_admin || has(p, 'org_admin', 'organisation', org);
}

export function canAdminWorkspace(p: Principal, ws: string, org: string): boolean {
	return canAdminOrg(p, org) || has(p, 'workspace_admin', 'workspace', ws);
}

/** The audit log of the installation: platform admins and platform auditors. */
export function canReadAuditLog(p: Principal): boolean {
	return p.platform_admin || has(p, 'auditor', 'platform');
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
	return (
		p.platform_admin ||
		p.roles.some(
			(r) => r.role === 'org_admin' || r.role === 'workspace_admin' || r.role === 'auditor'
		)
	);
}
