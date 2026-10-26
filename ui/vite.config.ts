import adapter from '@sveltejs/adapter-node';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';
import { CSP_DIRECTIVES } from './src/lib/csp.ts';

export default defineConfig({
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			adapter: adapter(),
			// Nonces on the pages it renders: every page of the console is rendered per request.
			csp: { mode: 'auto', directives: CSP_DIRECTIVES }
		})
	],
	build: {
		// Fonts and images stay files: the policy allows them from 'self', not as data: URLs.
		assetsInlineLimit: 0
	}
});
