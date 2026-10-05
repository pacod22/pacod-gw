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

/** "Platform admin", "Viewer of workspace ws_…": one line per role. */
export function describeRole(role: Principal['roles'][number]): string {
	const name = role.role.replace(/_/g, ' ');
	const label = name[0].toUpperCase() + name.slice(1);
	if (role.scope.type === 'platform') return `${label} of the platform`;
	return `${label} of ${role.scope.type} ${role.scope.id}`;
}
