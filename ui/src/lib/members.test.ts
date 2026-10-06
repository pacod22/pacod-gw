import { describe, expect, it } from 'vitest';
import { describeMemberRole, parseEmails } from './members';

describe('parseEmails', () => {
	it('splits on commas, semicolons and lines, and drops repeats', () => {
		expect(parseEmails(' Ann@x.test, ben@x.test;\nann@x.test\n\n carl@x.test ')).toEqual([
			'ann@x.test',
			'ben@x.test',
			'carl@x.test'
		]);
		expect(parseEmails('   ')).toEqual([]);
	});
});

describe('describeMemberRole', () => {
	it('names the workspace of a workspace role', () => {
		const base = { binding_id: 'rb_1', inherited: false };
		expect(
			describeMemberRole({
				...base,
				role: 'workspace_admin',
				scope: { type: 'workspace', id: 'ws_1' },
				scope_name: 'Billing'
			})
		).toBe('Workspace admin of Billing');
		expect(
			describeMemberRole({
				...base,
				role: 'org_admin',
				scope: { type: 'organisation', id: 'org_1' },
				scope_name: 'Acme'
			})
		).toBe('Organisation admin');
	});
});
