import { defineEnvVars } from '@sveltejs/kit/env';

/** An optional http(s) URL setting, without a trailing slash. */
function optionalUrl(name: string, example: string) {
	return (value: string | undefined) => {
		const text = (value ?? '').trim();
		if (text === '') return undefined;
		let url: URL;
		try {
			url = new URL(text);
		} catch {
			throw new Error(`${name} must be a URL such as ${example}, got "${text}"`);
		}
		if (url.protocol !== 'http:' && url.protocol !== 'https:') {
			throw new Error(`${name} must start with http:// or https://`);
		}
		return text.replace(/\/+$/, '');
	};
}

/** An optional secret: trimmed, empty when unset. */
const secret = (value: string | undefined) => (value ?? '').trim() || undefined;

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
					`PROMETHEUS_URL must be a URL such as http://pacod-prometheus:9090, got "${origin}"`
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
	PGW_AUTH: {
		description:
			"Who signs people in: local (the console's own login, the default) or oidc (pacod-id, with PGW_ID_ISSUER). The core's setting of the same name must match. Server-only.",
		schema: (value) => {
			const mode = (value ?? '').trim() || 'local';
			if (mode !== 'local' && mode !== 'oidc')
				throw new Error(`PGW_AUTH must be local or oidc, got "${mode}"`);
			return mode;
		}
	},
	PGW_ID_ISSUER: {
		description:
			"pacod-id's public issuer for the gateway's realm, such as https://id.example.com. With PGW_AUTH=oidc people sign in at pacod-id, and it and the PGW_UI_* settings are required. Server-only.",
		schema: optionalUrl('PGW_ID_ISSUER', 'https://id.example.com')
	},
	PGW_ID_INTERNAL_URL: {
		description:
			"pacod-id's internal listener as the console's server reaches it, such as http://pacod-id:8071: tokens, keys and sign-out go there. Default: the issuer. Server-only.",
		schema: optionalUrl('PGW_ID_INTERNAL_URL', 'http://pacod-id:8071')
	},
	PGW_UI_CLIENT_ID: {
		description: "The console's client at pacod-id. Default pacod-gw-console. Server-only.",
		schema: (value) => (value ?? '').trim() || 'pacod-gw-console'
	},
	PGW_UI_CLIENT_SECRET: {
		description: "The console's client secret at pacod-id. Server-only.",
		schema: secret
	},
	PGW_UI_SESSION_SECRET: {
		description:
			"At least 32 characters: seals the session cookie that holds pacod-id's tokens. Changing it signs everyone out of the console. Server-only.",
		schema: secret
	},
	PGW_CONSOLE_URL: {
		description:
			"The console's address as people reach it, such as https://console.example.com: where pacod-id sends them back after signing in and out. A request on another address is sent there first. Server-only.",
		schema: optionalUrl('PGW_CONSOLE_URL', 'https://console.example.com')
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
				throw new Error(`TESTER_URL must be a URL such as http://gw-tester:8095, got "${origin}"`);
			}
			if (url.protocol !== 'http:' && url.protocol !== 'https:') {
				throw new Error('TESTER_URL must start with http:// or https://');
			}
			return origin.replace(/\/+$/, '');
		}
	}
});
