import { defineConfig } from '@playwright/test';

// Run through scripts/e2e.sh, which starts pacod-id, the mail sink and the mock Google first.
// PID_E2E_CHROMIUM may name a Chromium already on the machine instead of Playwright's own.
export default defineConfig({
	testDir: '.',
	outputDir: './test-results',
	timeout: 60_000,
	// One service and one mailbox: the tests run one after the other.
	workers: 1,
	reporter: [['list']],
	use: {
		viewport: { width: 1280, height: 900 },
		locale: 'en-GB',
		launchOptions: {
			executablePath: process.env.PID_E2E_CHROMIUM || undefined,
			// Every *.e2e.test host is this machine: each realm its own host, its own cookies.
			args: ['--host-resolver-rules=MAP *.e2e.test 127.0.0.1']
		}
	}
});
