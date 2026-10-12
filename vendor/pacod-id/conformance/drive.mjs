#!/usr/bin/env node
// The browser of a conformance run. The suite's own browser (HtmlUnit) does not run the hosted
// pages' JavaScript, so the plans run without a "browser" block and this script plays the
// person instead: it asks the suite which URLs a running test wants visited, opens them in
// Chromium, signs in or confirms a sign-out on pacod-id's pages, and lets the suite's callback
// page report back. One browser context per test, so a test's second request sees the session
// of its first. Screenshots of every page that ends elsewhere go to .work/shots.
//
//   node conformance/drive.mjs        (run.sh starts and stops it)

import { mkdirSync } from 'node:fs';
import { chromium } from '@playwright/test';

// The suite's certificate is its own; this script talks to nothing else over TLS.
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';

// The suite's API by address; its pages by name (the name its redirect URIs carry).
const API = 'https://127.0.0.1:27443';
const SUITE = 'https://localhost.emobix.co.uk:27443';
const ID_HOST = 'pid.oidf.test:27470';
const EMAIL = 'oidf@pid.oidf.test';
const PASSWORD = 'the conformance passphrase';
const SHOTS = new URL('./.work/shots/', import.meta.url).pathname;
mkdirSync(SHOTS, { recursive: true });

const browser = await chromium.launch({
	executablePath: process.env.PID_E2E_CHROMIUM || undefined,
	args: ['--host-resolver-rules=MAP pid.oidf.test 127.0.0.1, MAP localhost.emobix.co.uk 127.0.0.1']
});
const contexts = new Map();
const busy = new Map();
let stopping = false;
process.on('SIGTERM', () => (stopping = true));
process.on('SIGINT', () => (stopping = true));

async function api(path, init) {
	const r = await fetch(API + path, init);
	if (!r.ok) return null;
	return r.status === 204 ? {} : r.json();
}

/** Fills the test's open screenshot placeholders with what the page shows now. */
async function fillPlaceholders(id, page) {
	const entries = (await api(`/api/log/${id}/images`).catch(() => null)) ?? [];
	const open = entries.filter((e) => e.upload && !e.img);
	if (!open.length) return;
	const png = await page.screenshot({ fullPage: true });
	const body = `data:image/png;base64,${png.toString('base64')}`;
	for (const e of open) {
		await fetch(`${API}/api/log/${id}/images/${e.upload}`, {
			method: 'POST',
			headers: { 'content-type': 'text/plain' },
			body
		});
		log(id, 'screenshot for', e.upload);
	}
}

const log = (id, ...what) => console.log(new Date().toISOString().slice(11, 19), id, ...what);

async function visit(id, url) {
	let ctx = contexts.get(id);
	if (!ctx) {
		ctx = await browser.newContext({ ignoreHTTPSErrors: true });
		contexts.set(id, ctx);
	}
	const page = await ctx.newPage();
	page.on('pageerror', (e) => log(id, 'page error', String(e).slice(0, 200)));
	page.on('requestfailed', (r) =>
		log(id, 'request failed', r.url().slice(0, 120), r.failure()?.errorText)
	);
	log(id, 'visit', url.slice(0, 160));
	try {
		// Once more after a transient failure (Chromium's ERR_NETWORK_CHANGED, for one).
		await page
			.goto(url, { waitUntil: 'load', timeout: 15_000 })
			.catch(() => page.goto(url, { waitUntil: 'load', timeout: 15_000 }));
		const deadline = Date.now() + 30_000;
		let signedIn = false;
		let said = 0;
		while (Date.now() < deadline) {
			if (Date.now() - said > 5000) {
				said = Date.now();
				log(id, 'at', page.url().slice(0, 100));
			}
			const u = new URL(page.url());
			if (u.host === new URL(SUITE).host) {
				// The suite's callback page posts what it received back to the suite.
				await page
					.waitForSelector('#submission_complete', { state: 'attached', timeout: 10_000 })
					.catch(() => {});
				log(id, 'back at the suite', u.pathname);
				break;
			}
			if (u.host === ID_HOST && u.pathname === '/login') {
				const email = page.locator('input[name=email]');
				if (!signedIn && (await email.isVisible().catch(() => false))) {
					// A test that asks for the sign-in page again wants to see it.
					await fillPlaceholders(id, page);
					await email.fill(EMAIL);
					await page.locator('input[name=password]').fill(PASSWORD);
					await page.locator('button[type=submit]').click();
					signedIn = true;
					log(id, 'signed in');
				}
			} else if (u.host === ID_HOST && u.pathname !== '/logout') {
				// pacod-id's error page (an unregistered redirect URI, a refused request), unless
				// the page is only passing through on its way back to the suite.
				await page.waitForTimeout(1500);
				if (page.url() === u.href) break;
				continue;
			} else if (u.host === ID_HOST && u.pathname === '/logout') {
				const confirm = page.locator('button.primary, button[data-variant=primary]').first();
				if (await confirm.isVisible().catch(() => false)) {
					await confirm.click();
					log(id, 'confirmed the sign-out');
					await page.waitForTimeout(1500);
				}
			}
			await page.waitForTimeout(250);
		}
		const u = new URL(page.url());
		if (u.host !== new URL(SUITE).host) {
			await fillPlaceholders(id, page);
			const shot = `${SHOTS}${id}-${Date.now()}.png`;
			await page.screenshot({ path: shot, fullPage: true });
			log(id, 'ended at', page.url().slice(0, 120), '->', shot);
		}
	} catch (e) {
		log(id, 'error', String(e).slice(0, 200));
	} finally {
		await page.close().catch(() => {});
	}
}

while (!stopping) {
	const running = (await api('/api/runner/running').catch(() => null)) ?? [];
	for (const id of running) {
		const b = await api(`/api/runner/browser/${id}`).catch(() => null);
		// The suite's pending URLs; marking one visited takes it off the list, and a test that
		// wants the same URL again (a second authorization) puts it back.
		for (const url of b?.urls ?? []) {
			await api(`/api/runner/browser/${id}/visit?url=${encodeURIComponent(url)}`, {
				method: 'POST'
			});
			// A test's visits run in order; different tests run side by side.
			const next = (busy.get(id) ?? Promise.resolve()).then(() => visit(id, url));
			busy.set(id, next);
		}
	}
	await new Promise((r) => setTimeout(r, 500));
}
await Promise.allSettled([...busy.values()]);
await browser.close();
