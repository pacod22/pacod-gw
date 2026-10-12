// The gateway's rules: the vectors next to the manifest (authz/vectors/), which the core's tests
// run too, and the console's own helpers on top of them.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { runVectors } from '@pacod/id-authz/vectors';
import { manifestFromYaml } from '@pacod/id-authz/yaml';
import type { Principal } from './api/types';
import {
	canAdminOrg,
	canAdminWorkspace,
	canReadAuditLog,
	canReadAuditRecords,
	canReadOrg,
	hasPlatformRole,
	subjectOf
} from './access';
import { manifest } from './authz.generated';

const dir = join(import.meta.dirname, '../../../authz');

const principal = (roles: Principal['roles']): Principal => ({
	id: 'usr_1',
	kind: 'user',
	display: 'u',
	roles,
	platform_admin: roles.some((r) => r.role === 'platform_admin'),
	reporting_currency: 'USD'
});

describe('the gateway rules', () => {
	it('hold the vectors', () => {
		const text = readFileSync(join(dir, 'vectors/gw.yaml'), 'utf8');
		const load = (name: string) =>
			manifestFromYaml(readFileSync(join(dir, `${name}.yaml`), 'utf8'));
		const { checks, failures } = runVectors(text, load);
		expect(failures).toEqual([]);
		expect(checks).toBeGreaterThan(60);
	});

	it('come from the manifest as it is on disk', () => {
		const onDisk = manifestFromYaml(readFileSync(join(dir, 'gw.yaml'), 'utf8'));
		expect(manifest.source).toEqual(onDisk.source);
	});
});

describe('access helpers', () => {
	const orgAdmin = principal([{ role: 'org_admin', scope: { type: 'organisation', id: 'o1' } }]);
	const wsAdmin = principal([
		{ role: 'workspace_admin', scope: { type: 'workspace', id: 'w1' }, organisation_id: 'o1' }
	]);
	const platformAuditor = principal([{ role: 'auditor', scope: { type: 'platform' } }]);

	it('place workspace roles under their organisation', () => {
		expect(subjectOf(wsAdmin).bindings).toEqual([
			{ role: 'gw.workspace_admin', path: ['platform', 'organisation:o1', 'gw.workspace:w1'] }
		]);
		// Without its organisation a workspace role grants nothing.
		const lost = principal([{ role: 'workspace_admin', scope: { type: 'workspace', id: 'w1' } }]);
		expect(subjectOf(lost).bindings).toEqual([]);
	});

	it('answer as the core does', () => {
		expect(canAdminOrg(orgAdmin, 'o1') && !canAdminOrg(orgAdmin, 'o2')).toBe(true);
		expect(canAdminWorkspace(wsAdmin, 'w1', 'o1') && !canReadOrg(wsAdmin, 'o1')).toBe(true);
		expect(hasPlatformRole(platformAuditor) && !hasPlatformRole(orgAdmin)).toBe(true);
		expect(canReadAuditRecords(wsAdmin) && canReadAuditRecords(platformAuditor)).toBe(true);
	});

	it('let organisation admins read their organisation’s audit log, as the core does', () => {
		expect(canReadAuditLog(orgAdmin, 'o1')).toBe(true);
		expect(canReadAuditLog(orgAdmin, 'o2')).toBe(false);
		expect(canReadAuditLog(orgAdmin)).toBe(false);
		expect(canReadAuditLog(platformAuditor)).toBe(true);
	});
});
