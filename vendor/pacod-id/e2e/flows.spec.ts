// Every flow of the hosted pages, end to end, against a running pacod-id (scripts/e2e.sh).

import {
	ADMIN,
	APP,
	REALMS,
	address,
	adminContext,
	authorize,
	expect,
	landsInApp,
	linkIn,
	call,
	mailTo,
	signIn,
	test
} from './fixtures';

test('a person signs up in shop, confirms by mail and comes back signed in', async ({ page }) => {
	const email = address('ana');
	const since = new Date().toISOString();
	await authorize(page, 'shop');
	await page.getByRole('link', { name: 'Create an account' }).click();
	await page.waitForURL(/\/signup\?flow=/);
	await page.getByLabel('Name').fill('Ana');
	await page.getByLabel('Email').fill(email);
	await page.getByLabel('Password', { exact: true }).fill('a long and unusual passphrase');
	// shop has terms of use: the form does not go without them.
	await page.getByRole('button', { name: 'Create account' }).click();
	await expect(page).toHaveURL(/\/signup\?flow=/);
	await page.getByLabel('I accept the terms of use').check();
	await page.getByRole('button', { name: 'Create account' }).click();
	await expect(page.getByRole('heading', { name: 'Confirm your email' })).toBeVisible();
	// The person tries to go on before confirming.
	await page.getByRole('button', { name: 'I have confirmed it' }).click();
	await expect(page.getByText('The address is not confirmed yet')).toBeVisible();
	const mail = await mailTo(page.request, email, since);
	expect(mail.subject).toContain('Shop');
	const confirm = await page.context().newPage();
	await confirm.goto(linkIn(mail.text));
	await expect(confirm.getByText(`${email} is confirmed.`)).toBeVisible();
	await expect(confirm, 'the token leaves the address bar').not.toHaveURL(/token=/);
	await confirm.close();
	await page.getByRole('button', { name: 'I have confirmed it' }).click();
	await landsInApp(page);
});

test('a person signs up in Bazaar, where eight characters are enough', async ({ page }) => {
	const email = address('vera');
	const since = new Date().toISOString();
	await authorize(page, 'bazaar', { prompt: 'create' });
	await expect(page).toHaveURL(/\/signup\?flow=/);
	await page.getByLabel('Email').fill(email);
	await page.getByLabel('Password', { exact: true }).fill('violetsky');
	await page.getByRole('button', { name: 'Create account' }).click();
	await expect(page.getByRole('heading', { name: 'Confirm your email' })).toBeVisible();
	const mail = await mailTo(page.request, email, since);
	expect(mail.subject, 'Bulgarian first, then English').toMatch(/Потвърдете.*·.*Confirm/);
	await page.goto(linkIn(mail.text));
	await expect(page.getByText(`${email} is confirmed.`)).toBeVisible();
});

test('the gateway admin signs in, finds the admin pages, and invites a colleague', async ({
	page,
	browser
}) => {
	await authorize(page, 'gateway');
	await expect(
		page.getByRole('link', { name: 'Create an account' }),
		'accounts come by invitation here'
	).toHaveCount(0);
	await signIn(page, ADMIN.email, ADMIN.password);
	await landsInApp(page);
	await page.goto(`${REALMS.gateway}/admin`);
	await expect(page.getByRole('heading', { name: 'Accounts', level: 1 })).toBeVisible();
	await expect(page.getByRole('cell', { name: ADMIN.email })).toBeVisible();

	// An invitation to the identity auditors, the way a product's facade sends one.
	const colleague = address('colleague');
	const since = new Date().toISOString();
	const r = await call(page, 'POST', 'admin/invitations', {
		email: colleague,
		role: 'id.auditor',
		scope: 'platform',
		labels: { role: 'Auditor', scope: 'the identity service' }
	});
	expect(r.status, JSON.stringify(r.body)).toBe(201);
	const mail = await mailTo(page.request, colleague, since, '/invite?token=');
	const invited = await browser.newContext();
	const p = await invited.newPage();
	await p.goto(linkIn(mail.text));
	await expect(p.getByText('invited you to the identity service as Auditor')).toBeVisible();
	await p.getByLabel('Choose a password').fill('short');
	await expect(
		p.getByText('Characters still needed: 7'),
		'a role holder needs twelve'
	).toBeVisible();
	await p.getByLabel('Choose a password').fill('a long passphrase for the colleague');
	await p.getByRole('button', { name: 'Create the account and accept' }).click();
	await expect(p.getByText('Your account is ready and the invitation is accepted.')).toBeVisible();
	// The new auditor signs in to the identity pages and sees the audit log.
	await p.goto(`${REALMS.gateway}/admin/audit`);
	await p.waitForURL(/\/login\?flow=/);
	await signIn(p, colleague, 'a long passphrase for the colleague');
	await p.waitForURL(/\/admin\/audit$/);
	await expect(p.getByRole('link', { name: 'Audit log' })).toBeVisible();
	await expect(p.getByRole('link', { name: 'Service accounts' })).toHaveCount(0);
	await invited.close();
});

test('a forgotten password: a link by mail, a new password, every session ends', async ({
	page,
	browser
}) => {
	const admin = await adminContext(browser);
	const email = address('ben');
	const created = await admin.post('admin/accounts', {
		email,
		password: 'the first long passphrase'
	});
	expect(created.status).toBe(201);
	expect((await admin.post(`admin/accounts/${created.body.id}/email-confirmation`)).status).toBe(
		204
	);

	const since = new Date().toISOString();
	await page.goto(`${REALMS.gateway}/forgot-password`);
	await page.getByLabel('Email').fill(email);
	await page.getByRole('button', { name: 'Send the link' }).click();
	await expect(page.getByText('a link is on its way')).toBeVisible();
	const mail = await mailTo(page.request, email, since, '/reset-password?token=');
	await page.goto(linkIn(mail.text));
	await expect(page, 'the token leaves the address bar').not.toHaveURL(/token=/);
	await page.getByLabel('New password').fill('password1234');
	await page.getByRole('button', { name: 'Save the new password' }).click();
	await expect(page.getByText('too common')).toBeVisible();
	await page.getByLabel('New password').fill('the second long passphrase');
	await page.getByRole('button', { name: 'Save the new password' }).click();
	await expect(page.getByText('Your password is changed')).toBeVisible();

	await authorize(page, 'gateway');
	await signIn(page, email, 'the first long passphrase');
	await expect(page.getByText('The email address or the password is wrong.')).toBeVisible();
	await signIn(page, email, 'the second long passphrase');
	await landsInApp(page);
	await admin.context.close();
});

test('a role holder with a short password chooses a longer one at the next sign-in', async ({
	page,
	browser
}) => {
	const admin = await adminContext(browser);
	const email = address('vlad');
	const since = new Date().toISOString();
	// A Bazaar sign-up with nine characters, confirmed.
	await authorize(page, 'bazaar', { prompt: 'create' });
	await page.getByLabel('Email').fill(email);
	await page.getByLabel('Password', { exact: true }).fill('ninechars');
	await page.getByRole('button', { name: 'Create account' }).click();
	await page.goto(linkIn((await mailTo(page.request, email, since, '/verify-email?token=')).text));
	await expect(page.getByText('is confirmed')).toBeVisible();
	// Then a role: now twelve characters are needed.
	const people = (await admin.get(`admin/accounts?q=${encodeURIComponent(email)}`)).body;
	const r = await admin.post('admin/bindings', {
		principal: people.accounts[0].id,
		role: 'id.auditor',
		scope: 'platform'
	});
	expect(r.status, JSON.stringify(r.body)).toBe(201);
	await authorize(page, 'bazaar');
	await signIn(page, email, 'ninechars');
	await expect(page.getByRole('heading', { name: 'Choose a new password' })).toBeVisible();
	await page.getByLabel('New password').fill('now a much longer passphrase');
	await page.getByRole('button', { name: 'Save and continue' }).click();
	await landsInApp(page);
	await admin.context.close();
});

test('ten wrong passwords lock a new browser out', async ({ page, browser }) => {
	const admin = await adminContext(browser);
	const email = address('lea');
	const id = (await admin.post('admin/accounts', { email, password: 'lea has a long passphrase' }))
		.body.id;
	await admin.post(`admin/accounts/${id}/email-confirmation`);
	await admin.context.close();
	await authorize(page, 'gateway');
	for (let i = 0; i < 10; i++) {
		await signIn(page, email, `wrong guess ${i}`);
		await expect(page.getByText(/password is wrong|Too many attempts/)).toBeVisible();
	}
	await signIn(page, email, 'lea has a long passphrase');
	await expect(page.getByText('Too many attempts')).toBeVisible();
});

test('devices: one ends another; signing out returns to the application', async ({
	page,
	browser
}) => {
	const email = address('dora');
	const admin = await adminContext(browser);
	const id = (await admin.post('admin/accounts', { email, password: 'dora has a long passphrase' }))
		.body.id;
	await admin.post(`admin/accounts/${id}/email-confirmation`);
	const other = await browser.newContext();
	const op = await other.newPage();
	for (const p of [page, op]) {
		await authorize(p, 'gateway');
		await signIn(p, email, 'dora has a long passphrase');
		await landsInApp(p);
	}
	await page.goto(`${REALMS.gateway}/account/sessions`);
	const rows = page.locator('tbody tr');
	await expect(rows).toHaveCount(2);
	await rows
		.filter({ hasNotText: 'this device' })
		.getByRole('button', { name: 'Sign out' })
		.click();
	await expect(rows).toHaveCount(1);
	await op.goto(`${REALMS.gateway}/account`);
	await op.waitForURL(/\/login\?flow=/);

	// RP-initiated sign-out without an id_token: the person confirms.
	const q = new URLSearchParams({
		client_id: 'demo-gw',
		post_logout_redirect_uri: `${APP}/gw/`,
		state: 'bye'
	});
	await page.goto(`${REALMS.gateway}/oidc/logout?${q}`);
	await page.waitForURL(/\/logout\?/);
	await page.getByRole('button', { name: 'Sign out' }).click();
	await page.waitForURL(`${APP}/gw/?state=bye`);
	await page.goto(`${REALMS.gateway}/account`);
	await page.waitForURL(/\/login\?flow=/);
	await other.close();
	await admin.context.close();
});

test('Bazaar signs a new person up with Google', async ({ page }) => {
	const email = address('gina');
	await authorize(page, 'bazaar');
	await page.getByRole('button', { name: 'Continue with Google' }).click();
	await page.waitForURL(/127\.0\.0\.1:27380\/authorize/);
	await page.getByLabel('Email').fill(email);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await landsInApp(page);
	await page.goto(`${REALMS.bazaar}/account/connections`);
	await expect(page.getByText(`Linked to ${email}`)).toBeVisible();
	await page.getByRole('button', { name: 'Unlink' }).click();
	await expect(page.getByText('only way to sign in')).toBeVisible();
});

test('the gateway refuses a Google address without an invitation', async ({ page }) => {
	await authorize(page, 'gateway');
	await page.getByRole('button', { name: 'Continue with Google' }).click();
	await page.waitForURL(/127\.0\.0\.1:27380\/authorize/);
	await page.getByLabel('Email').fill(address('stranger'));
	await page.getByRole('button', { name: 'Sign in' }).click();
	await page.waitForURL(/\/login\?flow=.*error=google_no_account/);
	await expect(page.getByText('There is no account for this Google address.')).toBeVisible();
});
