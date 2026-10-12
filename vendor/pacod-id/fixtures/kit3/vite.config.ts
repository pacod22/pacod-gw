import adapter from '@sveltejs/adapter-node';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		sveltekit({
			// adapter-node 6 takes the origin from here, fixed when building (`ORIGIN` is no longer
			// read when the server starts); without it, it assumes https.
			adapter: adapter({ out: process.env.FIXTURE_OUT ?? 'build' }),
			paths: { origin: process.env.FIXTURE_ORIGIN ?? 'http://kit3.e2e.test:27392' },
			// pacod-id's back-channel logout is a form post with its issuer as the Origin.
			csrf: {
				trustedOrigins: [
					new URL(process.env.ID_ISSUER ?? 'http://id.gateway.e2e.test:27370').origin
				]
			}
		})
	]
});
