/**
 * The installation's addresses and main settings, for platform admins on the System page:
 * what `make urls` prints on the host (deploy/scripts/urls.sh), so far as a service can know
 * it. The console sees no containers, only the settings it and the core were started with, so
 * every address here is a setting, and one that is not set is shown as such.
 */

/** The data plane's paths, after its address. */
export const DATA_PLANE_PATHS =
	'/v1 (OpenAI, Anthropic), /v1beta (Gemini), /gw/v1, /proxy/<provider>, /mcp, /a2a/<name>';

/** The command that prints Caddy's own root certificate, for PGW_TLS=internal. */
export const ROOT_CERTIFICATE =
	'docker exec pacod-caddy cat /data/caddy/pki/authorities/local/root.crt';

export interface InstallationInput {
	/** PGW_CONSOLE_URL, or the address the page was opened on. */
	consoleUrl: string;
	/** PGW_API_URL: URLs or host names, separated by spaces. */
	apiUrls?: string;
	/** PGW_ID_ISSUER. */
	idIssuer?: string;
	/** GRAFANA_URL. */
	grafanaUrl?: string;
	/** PGW_AUTH. */
	auth: string;
	/** PGW_TLS. */
	tls?: string;
	/** PGW_ADMIN_ALLOW_CIDRS. */
	adminCidrs?: string;
	/** PGW_BENCH_ADDRESS, <address>:<port>. */
	benchAddress?: string;
	/** The core's PGW_EGRESS_ALLOW_PRIVATE and PGW_TOOL_EGRESS_ALLOW_PRIVATE. */
	egressModels: string[];
	egressTools: string[];
}

export interface Fact {
	label: string;
	/** Empty: not set. */
	value: string;
	note?: string;
	/** A page people open in the browser, shown as a link. */
	link?: true;
}

export interface Installation {
	addresses: Fact[];
	/** The management API and the load tests' mock for a benchmark; null without BENCH=1. */
	bench: Fact[] | null;
	settings: Fact[];
	warnings: string[];
}

/** PGW_API_URL's entries as URLs: a host name is served on https. */
export function apiUrls(raw: string | undefined): string[] {
	return (raw ?? '')
		.split(/\s+/)
		.filter(Boolean)
		.map((entry) => (/^https?:\/\//i.test(entry) ? entry : `https://${entry}`))
		.map((url) => url.replace(/\/+$/, ''));
}

/** The host name of a URL, empty when it is not one. */
export function hostOf(url: string | undefined): string {
	if (!url) return '';
	try {
		return new URL(url).hostname;
	} catch {
		return '';
	}
}

/** Whether a list of CIDRs, separated by spaces or commas, admits every address. */
export function admitsEveryone(cidrs: string | undefined): boolean {
	return (cidrs ?? '').split(/[\s,]+/).some((c) => c === '0.0.0.0/0' || c === '::/0');
}

/** The benchmark's base URL and what its address means for who can reach it. */
function benchBase(address: string): { url: string; note: string } {
	const port = address.slice(address.lastIndexOf(':') + 1);
	const bind = address.slice(0, address.lastIndexOf(':'));
	if (bind === '0.0.0.0' || bind === '[::]' || bind === '')
		return {
			url: `http://<this host>:${port}`,
			note: `port ${port} on every interface of the host (PGW_BENCH_BIND=0.0.0.0)`
		};
	if (bind === '127.0.0.1')
		return { url: `http://${address}`, note: 'this host alone (PGW_BENCH_BIND)' };
	return { url: `http://${address}`, note: '' };
}

export function installation(input: InstallationInput): Installation {
	const api = apiUrls(input.apiUrls);
	const addresses: Fact[] = [{ label: 'Console', value: input.consoleUrl, link: true }];
	if (api.length === 0)
		addresses.push({ label: 'Data plane', value: '', note: 'PGW_API_URL is not set' });
	for (const url of api)
		addresses.push({ label: 'Data plane', value: url, note: DATA_PLANE_PATHS });
	if (input.idIssuer)
		addresses.push({ label: 'Identity (pacod-id)', value: input.idIssuer, link: true });
	const grafanaUnderConsole = /\/grafana(\/|$)/.test(input.grafanaUrl ?? '');
	if (input.grafanaUrl)
		addresses.push({
			label: 'Grafana',
			value: input.grafanaUrl,
			note: grafanaUnderConsole ? 'from PGW_ADMIN_ALLOW_CIDRS only' : undefined,
			link: true
		});

	let bench: Fact[] | null = null;
	if (input.benchAddress) {
		const { url, note } = benchBase(input.benchAddress);
		bench = [
			{ label: 'Management API', value: `${url}/api/v1/`, note },
			{ label: "Load tests' mock", value: `${url}/load-mock/` },
			{ label: 'Data plane', value: `${url}/v1` }
		];
	}

	const settings: Fact[] = [
		{
			label: 'Sign-in',
			value:
				input.auth === 'oidc'
					? 'Through pacod-id (PGW_AUTH=oidc)'
					: "The console's own (PGW_AUTH=local)"
		}
	];
	if (input.tls === 'internal')
		settings.push({
			label: 'Certificates',
			value: "Caddy's own certificate authority (PGW_TLS=internal)",
			note: `Browsers trust them once its root certificate is installed: ${ROOT_CERTIFICATE}`
		});
	else if (input.tls)
		settings.push({ label: 'Certificates', value: `Let's Encrypt, account ${input.tls}` });
	settings.push(
		{
			label: 'Private model hosts',
			value: input.egressModels.join(', ') || 'none',
			note: 'PGW_EGRESS_ALLOW_PRIVATE'
		},
		{
			label: 'Private tool hosts',
			value: input.egressTools.join(', ') || 'none',
			note: 'PGW_TOOL_EGRESS_ALLOW_PRIVATE'
		}
	);
	if (input.adminCidrs)
		settings.push({
			label: 'Admin addresses',
			value: input.adminCidrs,
			note: 'PGW_ADMIN_ALLOW_CIDRS: who reaches Grafana, and the management API with BENCH=1'
		});

	return { addresses, bench, settings, warnings: warningsOf(input, api, grafanaUnderConsole) };
}

/** What made the console unreachable once, and a management API open to every address. */
function warningsOf(
	input: InstallationInput,
	api: string[],
	grafanaUnderConsole: boolean
): string[] {
	const warnings: string[] = [];
	const hosts = [input.consoleUrl, ...api, input.idIssuer, input.grafanaUrl].map(hostOf);
	for (const host of new Set(hosts.filter((h) => h.endsWith('.local'))))
		warnings.push(
			`${host} ends in .local, which macOS and many Linux machines look up by multicast DNS, not through the network's DNS server. Use another domain, such as .lan or .internal.`
		);
	const grafana = hostOf(input.grafanaUrl);
	if (grafanaUnderConsole && grafana && grafana !== hostOf(input.consoleUrl))
		warnings.push(
			`Grafana's address names ${grafana}, not the console's host: its links and its sign-in lead there.`
		);
	if (input.benchAddress && admitsEveryone(input.adminCidrs))
		warnings.push(
			'PGW_ADMIN_ALLOW_CIDRS admits every address to the management API on the API site, over plain HTTP. List the machines that need it.'
		);
	return warnings;
}
