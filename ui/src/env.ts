import { defineEnvVars } from '@sveltejs/kit/env';

export const variables = defineEnvVars({
	GATEWAY_API_URL: {
		description:
			'Origin of the management listener of pacod-gw-core, without a path. Server-only: the browser never talks to the core.',
		schema: (value) => {
			const origin = (value ?? '').trim() || 'http://127.0.0.1:28081';
			let url: URL;
			try {
				url = new URL(origin);
			} catch {
				throw new Error(
					`GATEWAY_API_URL must be a URL such as http://127.0.0.1:28081, got "${origin}"`
				);
			}
			if (url.protocol !== 'http:' && url.protocol !== 'https:') {
				throw new Error('GATEWAY_API_URL must start with http:// or https://');
			}
			return origin.replace(/\/+$/, '');
		}
	},
	PROMETHEUS_URL: {
		description:
			'Origin of the Prometheus server that scrapes the gateway, without a path. Optional: without it the Metrics page says that it is not set up. Server-only.',
		schema: (value) => {
			const origin = (value ?? '').trim();
			if (origin === '') return undefined;
			let url: URL;
			try {
				url = new URL(origin);
			} catch {
				throw new Error(
					`PROMETHEUS_URL must be a URL such as http://prometheus:9090, got "${origin}"`
				);
			}
			if (url.protocol !== 'http:' && url.protocol !== 'https:') {
				throw new Error('PROMETHEUS_URL must start with http:// or https://');
			}
			return origin.replace(/\/+$/, '');
		}
	}
});
