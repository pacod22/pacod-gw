import { describe, expect, it } from 'vitest';
import { allowFormTarget, CSP_DIRECTIVES } from './csp.ts';

const POLICY =
	"default-src 'self'; script-src 'self' 'nonce-abc'; form-action 'self'; frame-ancestors 'none'";

describe('the console’s Content-Security-Policy', () => {
	it('runs no inline or evaluated script and is never framed', () => {
		const script = CSP_DIRECTIVES['script-src'] ?? [];
		expect(script).not.toContain('unsafe-inline');
		expect(script).not.toContain('unsafe-eval');
		expect(CSP_DIRECTIVES['default-src']).toEqual(['self']);
		expect(CSP_DIRECTIVES['frame-ancestors']).toEqual(['none']);
		expect(CSP_DIRECTIVES['object-src']).toEqual(['none']);
	});

	it('allows inline styles as attributes only', () => {
		expect(CSP_DIRECTIVES['style-src']).toEqual(['self']);
		expect(CSP_DIRECTIVES['style-src-attr']).toEqual(['unsafe-inline']);
	});

	it('sends forms to the console alone, unless the hook adds the issuer', () => {
		expect(CSP_DIRECTIVES['form-action']).toEqual(['self']);
	});
});

describe('the issuer as a form target', () => {
	it('is added to form-action and nowhere else', () => {
		expect(allowFormTarget(POLICY, 'https://id.example.com')).toBe(
			"default-src 'self'; script-src 'self' 'nonce-abc'; form-action 'self' https://id.example.com; frame-ancestors 'none'"
		);
	});

	it('leaves a response without a policy alone', () => {
		expect(allowFormTarget(null, 'https://id.example.com')).toBeNull();
		expect(allowFormTarget('', 'https://id.example.com')).toBeNull();
	});

	it('does not take a directive that only starts like form-action', () => {
		expect(allowFormTarget("form-actions 'self'", 'https://id.example.com')).toBe(
			"form-actions 'self'"
		);
	});
});
