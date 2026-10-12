import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

// The components are rendered on the server side (svelte/server) and their HTML checked; the
// token generator is checked against the JSON it reads. No browser is needed.
export default defineConfig({
	plugins: [svelte()],
	test: {
		include: ['src/**/*.test.ts'],
		environment: 'node'
	}
});
