import { createRawSnippet } from 'svelte';
import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import {
	AccountMenu,
	AuthCard,
	BrandMark,
	Button,
	Checkbox,
	Divider,
	LocaleSwitcher,
	Notice,
	PasswordField,
	ProviderButton,
	TextField,
	initials
} from '../lib/index.js';

const text = (s: string) => createRawSnippet(() => ({ render: () => `<p>${s}</p>` }));

describe('initials', () => {
	it('takes two letters of a name or an address', () => {
		expect(initials('Ada Lovelace')).toBe('AL');
		expect(initials('ada.lovelace@example.com')).toBe('AL');
		expect(initials('admin@example.com')).toBe('AD');
		expect(initials('Иван Петров')).toBe('ИП');
		expect(initials('')).toBe('?');
	});
});

describe('components', () => {
	it('AuthCard shows the brand mark, title and wordmark', () => {
		const { body, head } = render(AuthCard, {
			props: {
				title: 'Welcome back',
				subtitle: 'Sign in.',
				brand: 'shop',
				children: text('form here')
			}
		});
		expect(body).toContain('<h1');
		expect(body).toContain('Welcome back');
		expect(body).toContain('>SH<');
		expect(body).toContain('>Shop<');
		expect(body).toContain('form here');
		expect(head).toContain('noindex');
	});

	it('AuthCard defaults to the gateway brand, and the wordmark can be left out', () => {
		expect(render(AuthCard, { props: { title: 't', children: text('x') } }).body).toContain(
			'pacod-gw · AI Gateway'
		);
		const without = render(AuthCard, {
			props: { title: 't', wordmark: '', children: text('x') }
		}).body;
		expect(without).not.toContain('wordmark');
	});

	it('BrandMark takes a monogram of its own', () => {
		expect(render(BrandMark, { props: { brand: 'bazaar' } }).body).toContain('>B<');
		expect(render(BrandMark, { props: { monogram: 'ID' } }).body).toContain('>ID<');
	});

	it('Button is a link with href, and a disabled button while busy', () => {
		const link = render(Button, {
			props: { href: '/x', variant: 'primary', children: text('Go') }
		}).body;
		expect(link).toMatch(/<a[^>]+href="\/x"/);
		const busy = render(Button, {
			props: { busy: true, type: 'submit', children: text('Save') }
		}).body;
		expect(busy).toMatch(/<button[^>]*disabled/);
		expect(busy).toContain('aria-busy="true"');
	});

	it('TextField links its label, and an error marks the input invalid', () => {
		const ok = render(TextField, {
			props: { label: 'Email', name: 'email', type: 'email', hint: 'Work address' }
		}).body;
		const id = /<label for="([^"]+)"/.exec(ok)?.[1];
		expect(id).toBeTruthy();
		expect(ok).toContain(`id="${id}"`);
		expect(ok).toContain('Work address');
		const bad = render(TextField, {
			props: { label: 'Email', id: 'e', error: 'Not an address' }
		}).body;
		expect(bad).toContain('aria-invalid="true"');
		expect(bad).toContain('aria-describedby="e-note"');
		expect(bad).toContain('Not an address');
	});

	it('PasswordField counts the missing characters', () => {
		const html = render(PasswordField, {
			props: {
				label: 'Password',
				id: 'p',
				value: 'abc',
				minLength: 12,
				missing: (n: number) => `${n} more`
			}
		}).body;
		expect(html).toContain('9 more');
		expect(html).toContain('type="password"');
		expect(html).toContain('minlength="12"');
		expect(html).toContain('>Show<');
	});

	it('Checkbox, Notice, Divider, ProviderButton, LocaleSwitcher render their parts', () => {
		expect(
			render(Checkbox, { props: { label: 'Keep me signed in', name: 'r', checked: true } }).body
		).toContain('checked');
		expect(render(Notice, { props: { tone: 'error', children: text('Wrong') } }).body).toContain(
			'role="alert"'
		);
		expect(render(Notice, { props: { children: text('Done') } }).body).toContain('role="status"');
		expect(render(Divider, { props: { label: 'or' } }).body).toContain('>or<');
		expect(
			render(ProviderButton, { props: { provider: 'google', label: 'Continue with Google' } }).body
		).toContain('Continue with Google');
		const locales = render(LocaleSwitcher, {
			props: {
				locales: [
					{ code: 'en', label: 'English' },
					{ code: 'bg', label: 'Български' }
				],
				current: 'bg',
				href: (c: string) => `?ui_locales=${c}`
			}
		}).body;
		expect(locales).toContain('href="?ui_locales=en"');
		expect(locales).toMatch(
			/hreflang="bg"[^>]*aria-current="true"|aria-current="true"[^>]*hreflang="bg"/
		);
	});

	it('AccountMenu links to the account pages and signs out with a form post', () => {
		const html = render(AccountMenu, {
			props: {
				name: 'Ada Lovelace',
				email: 'ada@example.com',
				accountHref: 'https://id.example/account',
				signOutAction: '/auth/logout'
			}
		}).body;
		expect(html).toContain('>AL<');
		expect(html).toContain('href="https://id.example/account"');
		expect(html).toMatch(/<form[^>]+method="POST"[^>]+action="\/auth\/logout"/);
	});
});
