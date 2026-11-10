import {
	GRAFANA_URL,
	PGW_ADMIN_ALLOW_CIDRS,
	PGW_API_URL,
	PGW_AUTH,
	PGW_BENCH_ADDRESS,
	PGW_CONSOLE_URL,
	PGW_ID_ISSUER,
	PGW_TLS
} from '$app/env/private';
import type { ApiProblem } from '#lib/api/errors.ts';
import { installation } from '#lib/installation.ts';
import type { AgentsAnswer } from '#lib/load.ts';
import { problemOf } from '#lib/server/resource.ts';
import { tester, testerConfigured } from '#lib/server/tester.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [status, changes, live] = await Promise.all([
		event.locals.api.GET('/system/status'),
		event.locals.api.GET('/system/config-versions', { params: { query: { limit: 25 } } }),
		// What each instance is doing, joined into the instance list.
		event.locals.api.GET('/monitoring/live')
	]);
	let problem: ApiProblem | null = null;
	if (!status.response.ok) problem = problemOf(event, status);
	else if (!changes.response.ok) problem = problemOf(event, changes);
	// The installation's addresses and settings are for platform admins alone.
	const admin = event.locals.principal?.platform_admin === true;
	return {
		status: status.data ?? null,
		changes: changes.data ?? [],
		live: live.data ?? null,
		problem,
		installation:
			admin && status.data
				? installation({
						consoleUrl: PGW_CONSOLE_URL ?? event.url.origin,
						apiUrls: PGW_API_URL,
						idIssuer: PGW_ID_ISSUER,
						grafanaUrl: GRAFANA_URL,
						auth: PGW_AUTH,
						tls: PGW_TLS,
						adminCidrs: PGW_ADMIN_ALLOW_CIDRS,
						benchAddress: PGW_BENCH_ADDRESS,
						// A core of an earlier release, during a rolling upgrade, has neither list.
						egressModels: status.data.egress_allow_private ?? [],
						egressTools: status.data.tool_egress_allow_private ?? []
					})
				: null,
		// Streamed, so that the page does not wait: the tester asks each agent whether it is
		// there, which takes up to a few seconds. Never rejects; null when it cannot say.
		loadTests:
			admin && testerConfigured()
				? tester<AgentsAnswer>(event.locals, 'GET', '/load/agents').then(
						(answer) => answer.data,
						() => null
					)
				: null
	};
};
