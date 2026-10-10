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
	},
	GRAFANA_URL: {
		description:
			'Address of Grafana as the browser reaches it, such as https://console.example.com/grafana/. Optional: the Monitoring page links to it when it is set. The console only links to it.',
		schema: (value) => {
			const link = (value ?? '').trim();
			if (link === '') return undefined;
			let url: URL;
			try {
				url = new URL(link);
			} catch {
				throw new Error(
					`GRAFANA_URL must be a URL such as https://console.example.com/grafana/, got "${link}"`
				);
			}
			if (url.protocol !== 'http:' && url.protocol !== 'https:') {
				throw new Error('GRAFANA_URL must start with http:// or https://');
			}
			return link;
		}
	},
	TESTER_URL: {
		description:
			'Origin of the tester service, without a path. Optional: without it the Testing page says that testing is not set up. Server-only.',
		schema: (value) => {
			const origin = (value ?? '').trim();
			if (origin === '') return undefined;
			let url: URL;
			try {
				url = new URL(origin);
			} catch {
				throw new Error(`TESTER_URL must be a URL such as http://tester:8095, got "${origin}"`);
			}
			if (url.protocol !== 'http:' && url.protocol !== 'https:') {
				throw new Error('TESTER_URL must start with http:// or https://');
			}
			return origin.replace(/\/+$/, '');
		}
	}
});
