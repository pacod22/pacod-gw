import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
	consoleCss,
	consoleNames,
	readTokens,
	TAILWIND,
	tailwindCss,
	tokensCss
} from '../../scripts/tokens.js';
import { BRANDS } from '../lib/brands.js';

const tokens = readTokens(join(import.meta.dirname, '../../tokens'));
const css = tokensCss(tokens);
const defined = new Set([...css.matchAll(/--pid-([a-z0-9-]+):/g)].map((m) => m[1]));

describe('tokens.css', () => {
	it('writes light and dark values in one declaration', () => {
		expect(css).toContain('--pid-bg: light-dark(#fbfbf8, #0f1117);');
		expect(css).toContain('--pid-gold: #bf9540;');
		expect(css).toContain("--pid-font: 'Inter Variable', 'Inter'");
		expect(css).toContain("--pid-font-display: 'Manrope Variable'");
		expect(css).toContain('color-scheme: light dark;');
		expect(css).toContain(":root[data-theme='dark'] {\n\tcolor-scheme: dark;\n}");
	});

	it('gives Bazaar its own palette and fonts, and repeats the derived tokens there', () => {
		const bazaar = css.slice(css.indexOf(":root[data-brand='bazaar']"));
		expect(bazaar).toContain('--pid-accent: light-dark(#a2542a, #c67139);');
		expect(bazaar).toContain("--pid-font-display: 'Caprasimo', Georgia, serif;");
		expect(bazaar).toContain('--pid-focus-ring:');
		// The gateway, the CMS, the knowledge base and Shop share the base look: no block of their own.
		expect(css).not.toContain("data-brand='gateway'");
		expect(css).not.toContain("data-brand='cms'");
		expect(css).not.toContain("data-brand='kb'");
		expect(css).not.toContain("data-brand='shop'");
	});

	it('keeps the brand names in step with tokens/brands.json', () => {
		for (const [brand, b] of Object.entries(BRANDS)) {
			expect(tokens.brands[brand].monogram).toBe(b.monogram);
			expect(tokens.brands[brand].wordmark).toBe(b.wordmark);
		}
	});
});

describe('console.css', () => {
	it('aliases every console name to the canonical token', () => {
		const out = consoleCss(tokens);
		for (const name of consoleNames(tokens)) {
			expect(defined.has(name), name).toBe(true);
			expect(out).toContain(`--${name}: var(--pid-${name});`);
		}
		// The names the console's app.css uses today.
		for (const name of [
			'bg',
			'surface',
			'accent',
			'on-accent',
			'focus-ring',
			'shadow-sm',
			'radius',
			'sidebar-active-text',
			'series-other',
			'control-height'
		]) {
			expect(out).toContain(`--${name}: var(--pid-${name});`);
		}
	});
});

describe('tailwind.css', () => {
	it("maps shop's theme names onto existing tokens", () => {
		const out = tailwindCss();
		expect(out).toContain('@theme inline {');
		for (const [name, token] of TAILWIND) {
			expect(defined.has(token), token).toBe(true);
			expect(out).toContain(`--${name}: var(--pid-${token});`);
		}
		expect(out).toContain('--color-surface: var(--pid-bg);');
		expect(out).toContain('--color-brand: var(--pid-accent);');
	});
});
