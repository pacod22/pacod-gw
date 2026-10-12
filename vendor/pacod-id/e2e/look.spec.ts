// A tour of the pages in each brand, light and dark, with screenshots for a person to review
// (e2e/screenshots). The tour fails on console errors and CSP violations like every test.

import {
	REALMS,
	adminContext,
	authorize,
	expect,
	landsInApp,
	signIn,
	test,
	type RealmName
} from './fixtures';

const SHOTS = new URL('./screenshots/', import.meta.url).pathname;
const PERSON = { email: 'look@e2e.test', password: 'the look tour passphrase' };

test.beforeAll(async ({ browser }) => {
	const admin = await adminContext(browser);
	const r = await admin.post('admin/accounts', {
		email: PERSON.email,
		display_name: 'Lou Look',
		password: PERSON.password
	});
	if (r.status === 201) await admin.post(`admin/accounts/${r.body.id}/email-confirmation`);
	await admin.context.close();
});

for (const scheme of ['light', 'dark'] as const) {
	test.describe(scheme, () => {
		test.use({ colorScheme: scheme });
		for (const realm of Object.keys(REALMS) as RealmName[]) {
			test(`${realm}`, async ({ page }) => {
				const shot = async (name: string) => {
					await page.waitForLoadState('networkidle');
					await page.screenshot({ path: `${SHOTS}${realm}-${scheme}-${name}.png`, fullPage: true });
				};
				await authorize(page, realm);
				await shot('login');
				if (realm !== 'gateway') {
					await page.getByRole('link', { name: 'Create an account' }).click();
					await page.waitForURL(/\/signup\?flow=/);
					await shot('signup');
					await page.getByRole('link', { name: 'I have an account' }).click();
					await page.waitForURL(/\/login\?flow=/);
				}
				await signIn(
					page,
					realm === 'gateway' ? 'root@e2e.test' : PERSON.email,
					realm === 'gateway' ? 'the e2e bootstrap passphrase' : PERSON.password
				);
				await landsInApp(page);
				for (const path of [
					'/account',
					'/account/security',
					'/account/sessions',
					'/account/organisations'
				]) {
					await page.goto(`${REALMS[realm]}${path}`);
					await expect(page.locator('main h1')).toBeVisible();
					await shot(path.slice(1).replaceAll('/', '-'));
				}
				if (realm === 'gateway') {
					for (const path of ['/admin', '/admin/service-accounts', '/admin/audit']) {
						await page.goto(`${REALMS[realm]}${path}`);
						await expect(page.locator('main h1')).toBeVisible();
						await shot(path.slice(1).replaceAll('/', '-'));
					}
				}
				await page.goto(`${REALMS[realm]}/forgot-password`);
				await shot('forgot-password');
				await page.goto(`${REALMS[realm]}/logout`);
				await shot('logout');
			});
		}
	});
}
