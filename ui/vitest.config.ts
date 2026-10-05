import { defineConfig } from 'vitest/config';

// Unit tests cover the pure logic of the console (schema to form, error mapping, timing,
// navigation). They need neither SvelteKit nor a browser.
export default defineConfig({
	test: {
		include: ['src/**/*.test.ts'],
		environment: 'node'
	}
});
