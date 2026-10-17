// What the end-to-end tests share: the realms, the application the sign-ins return to (answered
// by the browser itself), the mail sink, and a check that no page logs an error or breaks its
// Content-Security-Policy.

import {
	test as base,
	expect,
	type APIRequestContext,
	type Browser,
	type Page
} from '@playwright/test';
import crypto from 'node:crypto';

export const PORT = 27370;
export const REALMS = {
	gateway: `http://id.gateway.e2e.test:${PORT}`,
	shop: `http://id.shop.e2e.test:${PORT}`,
	bazaar: `http://id.bazaar.e2e.test:${PORT}`
} as const;
export type RealmName = keyof typeof REALMS;
export const CLIENTS: Record<RealmName, string> = {
	gateway: 'demo-gw',
	shop: 'demo-shop',
	bazaar: 'demo-bazaar'
};
/** The path of each realm's application, where its sign-ins return. */
const APP_PATH: Record<RealmName, string> = { gateway: 'gw', shop: 'shop', bazaar: 'bazaar' };
export const APP = 'http://app.e2e.test:27399';
export const MAIL = 'http://127.0.0.1:27326';
export const ADMIN = { email: 'root@e2e.test', password: 'the e2e bootstrap passphrase' };

export const test = base.extend<{ problems: string[] }>({
	problems: [
		async ({ page }, use) => {
			const problems: string[] = [];
			page.on('pageerror', (e) => problems.push(`page error: ${e.message}`));
			page.on('console', (m) => {
				// Answers like 401 for a wrong password are expected; anything else is not.
				if (m.type() === 'error' && !/the server responded with a status of 4\d\d/.test(m.text())) {
					problems.push(`console: ${m.text().slice(0, 300)}`);
				}
			});
			await use(problems);
			expect(problems, 'console errors and CSP violations').toEqual([]);
		},
		{ auto: true }
	]
});
export { expect };

/** Starts a sign-in at the realm's application client; the page lands on the login (or signup). */
export async function authorize(page: Page, realm: RealmName, extra: Record<string, string> = {}) {
	const verifier = crypto.randomBytes(32).toString('base64url');
	const challenge = crypto.createHash('sha256').update(verifier).digest('base64url');
	const q = new URLSearchParams({
		response_type: 'code',
		client_id: CLIENTS[realm],
		redirect_uri: `${APP}/${APP_PATH[realm]}/callback`,
		scope: 'openid email profile',
		state: 'st',
		nonce: 'n',
		code_challenge: challenge,
		code_challenge_method: 'S256',
		...extra
	});
	await page.goto(`${REALMS[realm]}/oidc/authorize?${q}`);
	await page.waitForURL(/\/(login|signup)\?flow=/);
}

/** Signs in on the login page; the browser goes back to the application with a code. */
export async function signIn(page: Page, email: string, password: string) {
	await page.getByLabel('Email').fill(email);
	await page.getByLabel('Password', { exact: true }).fill(password);
	await page.getByRole('button', { name: 'Sign in' }).click();
}

export async function landsInApp(page: Page) {
	await page.waitForURL(new RegExp(`^${APP}/.*[?&]code=`));
}

interface MailItem {
	ID: string;
	Subject: string;
	To: { Address: string }[];
}

/**
 * The newest mail to `to` received after `since` (an ISO time) whose text contains `contains`,
 * waiting up to 15 seconds: the service sends mail every few seconds.
 */
export async function mailTo(
	request: APIRequestContext,
	to: string,
	since: string,
	contains = ''
): Promise<{ subject: string; text: string }> {
	for (let i = 0; i < 60; i++) {
		const r = await request.get(
			`${MAIL}/api/v1/search?query=${encodeURIComponent(`to:"${to}" after:"${since}"`)}`
		);
		const list = (await r.json()) as { messages: MailItem[] };
		for (const item of list.messages ?? []) {
			const m = await (await request.get(`${MAIL}/api/v1/message/${item.ID}`)).json();
			if ((m.Text as string).includes(contains))
				return { subject: m.Subject as string, text: m.Text as string };
		}
		await new Promise((res) => setTimeout(res, 250));
	}
	throw new Error(`no mail to ${to} with ${contains}`);
}

/** The link of a mail. */
export function linkIn(text: string): string {
	const m = text.match(/https?:\/\/\S+token=[A-Za-z0-9_-]+/);
	if (!m) throw new Error(`no link in: ${text}`);
	return m[0];
}

/** An answer of the API: its status and its JSON. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any -- the tests read whatever the API answers
type Answer = { status: number; body: any };

/** A call to /ui-api/v1 from inside the page, as the page's own scripts make it. */
export async function call(
	page: Page,
	method: string,
	path: string,
	data?: unknown
): Promise<Answer> {
	return page.evaluate(
		async ({ method, path, data }) => {
			const r = await fetch(`/ui-api/v1/${path}`, {
				method,
				headers: data === undefined ? {} : { 'content-type': 'application/json' },
				body: data === undefined ? undefined : JSON.stringify(data)
			});
			const text = await r.text();
			return { status: r.status, body: text ? JSON.parse(text) : null };
		},
		{ method, path, data }
	);
}

/** The bootstrap admin, signed in to the gateway realm in a context of its own. */
export async function adminContext(browser: Browser) {
	const context = await browser.newContext();
	const page = await context.newPage();
	await authorize(page, 'gateway');
	await signIn(page, ADMIN.email, ADMIN.password);
	await landsInApp(page);
	await page.goto(`${REALMS.gateway}/account`);
	return {
		context,
		post: (path: string, data: unknown = {}) => call(page, 'POST', path, data),
		get: (path: string) => call(page, 'GET', path)
	};
}

/** A unique address for a test. */
export function address(name: string) {
	return `${name}.${crypto.randomBytes(3).toString('hex')}@e2e.test`;
}
