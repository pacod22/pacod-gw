import { describe, expect, it } from 'vitest';
import { identityRedirect, safeNext, signInPath } from './identity.ts';

const ISSUER = 'https://id.example.com';
const at = (path: string, method = 'GET') =>
	identityRedirect(ISSUER, new URL(path, 'https://console.example.com'), method);

describe('signing in through pacod-id', () => {
	it('sends the pages of the console’s own login to pacod-id’s', () => {
		expect(at('/login?next=%2Forgs%2Forg_1')).toBe('/auth/login?next=%2Forgs%2Forg_1');
		expect(at('/login')).toBe('/auth/login?next=%2F');
		expect(at('/forgot-password')).toBe(`${ISSUER}/forgot-password`);
		expect(at('/reset-password?token=old')).toBe(`${ISSUER}/forgot-password`);
		expect(at('/verify-email?token=old')).toBe(`${ISSUER}/account`);
		expect(at('/account/password')).toBe(`${ISSUER}/account/security`);
	});

	it('forwards old invitation links with their token, which pacod-id took over', () => {
		expect(at('/invite?token=a+b%2Fc')).toBe(`${ISSUER}/invite?token=a+b%2Fc`);
		expect(at('/invite')).toBe(`${ISSUER}/account/organisations`);
	});

	it('signs out by the package’s route, and only by a POST', () => {
		expect(at('/logout', 'POST')).toBe('/auth/logout');
		expect(at('/logout')).toBe('/');
	});

	it('leaves every other path alone', () => {
		for (const path of ['/', '/orgs', '/auth/login', '/events', '/no-access', '/loginx']) {
			expect(at(path)).toBeNull();
		}
	});

	it('returns to paths of the console only, and asks for the password after a refusal', () => {
		expect(safeNext('//evil.example')).toBe('/');
		expect(safeNext('https://evil.example')).toBe('/');
		expect(safeNext('/auth/callback')).toBe('/');
		expect(safeNext('/orgs?x=1')).toBe('/orgs?x=1');
		expect(signInPath('/orgs', true)).toBe('/auth/login?next=%2Forgs&prompt=login');
		expect(signInPath('/\\evil')).toBe('/auth/login?next=%2F');
	});
});
