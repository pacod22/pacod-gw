import { paraglideVitePlugin } from '@inlang/paraglide-js';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		// The messages are ui/messages/*.json, which the service's mails read too.
		paraglideVitePlugin({
			project: './project.inlang',
			outdir: './src/lib/paraglide',
			strategy: ['custom-pidlang', 'baseLocale']
		}),
		sveltekit({
			compilerOptions: {
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			// One page for every route: pacod-id serves it with the realm's brand and language
			// filled in (crates/pacod-id/src/ui.rs).
			adapter: adapter({ fallback: 'index.html', strict: false }),
			// Absolute asset paths: the same page answers /login and /account/sessions.
			paths: { relative: false }
		})
	],
	// Every asset a file of its own: the pages' CSP allows fonts and images from 'self', not data:.
	build: { assetsInlineLimit: 0 },
	// Development: the pages call the service on the same origin.
	server: {
		proxy: {
			'/ui-api': 'http://127.0.0.1:13070',
			'/oidc': 'http://127.0.0.1:13070',
			'/federation': 'http://127.0.0.1:13070'
		}
	}
});
