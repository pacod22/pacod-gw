import { describe, expect, it } from 'vitest';
import {
	admitsEveryone,
	apiUrls,
	DATA_PLANE_PATHS,
	installation,
	type InstallationInput
} from './installation';

/** A test machine on a local network, as it was set up when the console could not be reached. */
const CTEST: InstallationInput = {
	consoleUrl: 'https://console.ctest.local',
	apiUrls: 'https://api.ctest.local',
	idIssuer: 'https://id.ctest.lan',
	grafanaUrl: 'https://console.localhost/grafana/',
	auth: 'oidc',
	tls: 'internal',
	adminCidrs: '0.0.0.0/0',
	benchAddress: '0.0.0.0:8088',
	egressModels: ['gw-load-mock'],
	egressTools: ['gw-load-mock']
};

/** The development stack on this machine. */
const DEV: InstallationInput = {
	consoleUrl: 'http://127.0.0.1:13000',
	apiUrls: 'http://127.0.0.1:18080',
	idIssuer: 'http://127.0.0.1:13070',
	grafanaUrl: 'http://localhost:13001/',
	auth: 'local',
	egressModels: ['gw-mock', 'gw-load-mock', 'host.docker.internal'],
	egressTools: []
};

const value = (facts: { label: string; value: string }[], label: string) =>
	facts.filter((f) => f.label === label).map((f) => f.value);

describe('installation', () => {
	it('lists the addresses, the benchmark routes and the settings', () => {
		const i = installation(CTEST);
		expect(value(i.addresses, 'Console')).toEqual(['https://console.ctest.local']);
		expect(value(i.addresses, 'Data plane')).toEqual(['https://api.ctest.local']);
		expect(i.addresses.find((f) => f.label === 'Data plane')?.note).toBe(DATA_PLANE_PATHS);
		expect(value(i.addresses, 'Identity (pacod-id)')).toEqual(['https://id.ctest.lan']);
		expect(i.bench?.map((f) => f.value)).toEqual([
			'http://<this host>:8088/api/v1/',
			'http://<this host>:8088/load-mock/',
			'http://<this host>:8088/v1'
		]);
		expect(i.bench?.[0].note).toContain('every interface');
		expect(value(i.settings, 'Sign-in')).toEqual(['Through pacod-id (PGW_AUTH=oidc)']);
		expect(i.settings.find((f) => f.label === 'Certificates')?.note).toContain('root.crt');
		expect(value(i.settings, 'Private model hosts')).toEqual(['gw-load-mock']);
		expect(value(i.settings, 'Admin addresses')).toEqual(['0.0.0.0/0']);
	});

	it('warns about .local names, a Grafana on another host and an open management API', () => {
		const warnings = installation(CTEST).warnings;
		expect(warnings).toHaveLength(4);
		expect(warnings[0]).toMatch(/^console\.ctest\.local ends in \.local/);
		expect(warnings[1]).toMatch(/^api\.ctest\.local ends in \.local/);
		expect(warnings[2]).toContain('names console.localhost');
		expect(warnings[3]).toContain('admits every address');
	});

	it('has nothing to warn about once the names are right', () => {
		const fixed: InstallationInput = {
			...CTEST,
			consoleUrl: 'https://console.ctest.lan',
			apiUrls: 'api.ctest.lan',
			grafanaUrl: 'https://console.ctest.lan/grafana/',
			adminCidrs: '10.0.0.0/24'
		};
		const i = installation(fixed);
		expect(i.warnings).toEqual([]);
		expect(value(i.addresses, 'Data plane')).toEqual(['https://api.ctest.lan']);
	});

	it('takes .localhost for what it is, and an open allow list without the benchmark as fine', () => {
		const i = installation({
			...DEV,
			consoleUrl: 'https://console.localhost',
			adminCidrs: '0.0.0.0/0'
		});
		expect(i.warnings).toEqual([]);
	});

	it('shows the development stack without benchmark routes or certificates', () => {
		const i = installation(DEV);
		expect(i.bench).toBeNull();
		expect(i.settings.some((f) => f.label === 'Certificates')).toBe(false);
		expect(i.settings.some((f) => f.label === 'Admin addresses')).toBe(false);
		expect(value(i.settings, 'Private tool hosts')).toEqual(['none']);
		expect(i.addresses.find((f) => f.label === 'Grafana')?.note).toBeUndefined();
		expect(i.warnings).toEqual([]);
	});

	it('says when the data plane is not set, and names a Let’s Encrypt account', () => {
		const i = installation({ ...DEV, apiUrls: undefined, tls: 'ops@example.com' });
		expect(i.addresses.find((f) => f.label === 'Data plane')).toEqual({
			label: 'Data plane',
			value: '',
			note: 'PGW_API_URL is not set'
		});
		expect(value(i.settings, 'Certificates')).toEqual(["Let's Encrypt, account ops@example.com"]);
	});

	it('marks a benchmark port bound to this host alone', () => {
		const i = installation({ ...DEV, benchAddress: '127.0.0.1:8088' });
		expect(i.bench?.[0]).toEqual({
			label: 'Management API',
			value: 'http://127.0.0.1:8088/api/v1/',
			note: 'this host alone (PGW_BENCH_BIND)'
		});
	});
});

describe('apiUrls', () => {
	it('takes URLs and host names, separated by spaces', () => {
		expect(apiUrls(' http://127.0.0.1:18080  api.localhost https://10.0.0.150/ ')).toEqual([
			'http://127.0.0.1:18080',
			'https://api.localhost',
			'https://10.0.0.150'
		]);
		expect(apiUrls(undefined)).toEqual([]);
	});
});

describe('admitsEveryone', () => {
	it('finds 0.0.0.0/0 and ::/0 among other ranges', () => {
		expect(admitsEveryone('10.0.0.0/8 0.0.0.0/0')).toBe(true);
		expect(admitsEveryone('10.0.0.0/8,::/0')).toBe(true);
		expect(admitsEveryone('private_ranges')).toBe(false);
		expect(admitsEveryone(undefined)).toBe(false);
	});
});
