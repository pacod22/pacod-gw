// The SvelteKit fixture applications signing in with @pacod/id-sveltekit (scripts/e2e.sh):
// SvelteKit 2 with sessions in memory, SvelteKit 3 with sealed cookies, and the same SvelteKit 3
// build in a container that reaches pacod-id through its internal address only.

import { ADMIN, REALMS, expect, signIn, test } from './fixtures';

const APPS = {
	kit2: 'http://kit2.e2e.test:27391',
	kit3: 'http://kit3.e2e.test:27392',
	'kit3 in a container': 'http://kit3d.e2e.test:27393'
};

for (const [name, app] of Object.entries(APPS)) {
	test.describe(name, () => {
		test('signs in, refreshes once for parallel requests, and hears of a sign-out elsewhere', async ({
			page
		}) => {
			await page.goto(app);
			await expect(page.locator('#who')).toHaveText('signed out');
			await page.getByRole('link', { name: 'Sign in' }).click();
			await page.waitForURL(new RegExp(`^${REALMS.gateway}/login\\?flow=`));
			await signIn(page, ADMIN.email, ADMIN.password);
			await page.waitForURL(`${app}/`);
			await expect(page.locator('#who')).toContainText(ADMIN.email);

			// The fixtures refresh a second after each refresh (REFRESH_BEFORE=299 of 300 s): ten
			// parallel requests then share one refresh, and the session lives on.
			const first = await page.evaluate(() => fetch('/api/me').then((r) => r.json()));
			await page.waitForTimeout(2500);
			const answers = await page.evaluate(() =>
				Promise.all(
					Array.from({ length: 10 }, () =>
						fetch('/api/me').then(async (r) => ({ status: r.status, body: await r.json() }))
					)
				)
			);
			expect(answers.map((a) => a.status)).toEqual(Array(10).fill(200));
			const tokens = new Set(answers.map((a) => a.body.access));
			expect(tokens.size, 'one refresh for all ten').toBe(1);
			expect([...tokens][0]).not.toBe(first.access);
			await page.waitForTimeout(2500);
			const later = await page.evaluate(() => fetch('/api/me').then((r) => r.status));
			expect(later, 'no refresh token was taken for stolen').toBe(200);

			// Signing out on pacod-id's own pages: the application hears of it by back-channel.
			await page.goto(`${REALMS.gateway}/account/sessions`);
			const mine = page.locator('tbody tr').filter({ hasText: 'this device' });
			await mine.getByRole('button', { name: 'Sign out' }).click();
			// The page leaves once pacod-id has ended the session (leaving earlier cancels it).
			await page.waitForURL((u) => !u.pathname.endsWith('/sessions'));
			// The page's own fetch: the browser knows the test host names, Node does not.
			await page.goto(app);
			await expect
				.poll(async () => page.evaluate(() => fetch('/api/me').then((r) => r.status)), {
					timeout: 15_000,
					message: 'back-channel logout'
				})
				.toBe(401);
			await page.reload();
			await expect(page.locator('#who')).toHaveText('signed out');
		});

		test('signs out from the application, at pacod-id too', async ({ page }) => {
			await page.goto(`${app}/auth/login?next=/`);
			await signIn(page, ADMIN.email, ADMIN.password);
			await page.waitForURL(`${app}/`);
			await page.getByRole('button', { name: 'Sign out' }).click();
			await page.waitForURL(new RegExp(`^${app}/(\\?.*)?$`));
			await expect(page.locator('#who')).toHaveText('signed out');
			// The single sign-on session ended with it.
			await page.goto(`${REALMS.gateway}/account`);
			await page.waitForURL(/\/login\?flow=/);
		});
	});
}
