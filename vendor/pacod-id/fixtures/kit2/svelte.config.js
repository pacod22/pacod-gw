import adapter from '@sveltejs/adapter-node';

/** @type {import('@sveltejs/kit').Config} */
export default {
	kit: {
		adapter: adapter(),
		// pacod-id's back-channel logout is a form post with its issuer as the Origin.
		csrf: {
			trustedOrigins: [new URL(process.env.ID_ISSUER ?? 'http://id.gateway.e2e.test:27370').origin]
		}
	}
};
