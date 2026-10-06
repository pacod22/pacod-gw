/**
 * Reports: the analytics, monitoring and audit lists of the management API. The core
 * forwards them from the analytics service and the OpenAPI document leaves their answers
 * open, so a report is loaded as plain data and shown by components that read its rows.
 */
import { error } from '@sveltejs/kit';
import { GATEWAY_API_URL } from '$app/env/private';
import { toProblem, type ApiProblem } from '#lib/api/errors.ts';
import { toRfc3339 } from '#lib/schema/form.ts';
import { API_BASE, loose } from './api.ts';
import { problemOf, queryFromUrl, type KitEvent } from './resource.ts';

export interface Report {
	data: Record<string, unknown> | null;
	problem: ApiProblem | null;
	/** The query the report was asked with, as the filter form shows it. */
	filters: Record<string, string>;
}

/** The filters every usage report takes. */
export const SCOPE = ['organisation_id', 'workspace_id'];
export const RANGE = ['from', 'to'];
export const USAGE_FILTERS = [
	...RANGE,
	...SCOPE,
	'provider',
	'model',
	'template',
	'key_id',
	'end_user',
	'agent_id',
	'user_id',
	'task',
	'complexity',
	'routing_scheme',
	'routing_rule',
	'tag.{key}'
];

/** The parameters of the filter form that are not sent as they are. */
const FORM_ONLY = ['tag_key', 'tag_value', 'group_tag'];

/**
 * Tag filters of the page's URL: `tag.<key>=value` as the API takes them, or a key and a
 * value typed into two fields when the keys are not known.
 */
export function tagFilters(url: URL): Record<string, string> {
	const out: Record<string, string> = {};
	for (const [name, value] of url.searchParams) {
		if (name.startsWith('tag.') && name.length > 4 && value !== '') out[name] = value;
	}
	const key = url.searchParams.get('tag_key')?.trim();
	const value = url.searchParams.get('tag_value') ?? '';
	if (key && value !== '') out[`tag.${key}`] = value;
	return out;
}

/** What a report is asked with, and what the filter form shows. */
export function reportQuery(
	url: URL,
	allowed: string[],
	defaults: Record<string, string> = {}
): { query: Record<string, string>; filters: Record<string, string> } {
	const filters = queryFromUrl(allowed, url, defaults);
	const query = { ...filters };
	// The filter form gives times without a zone; they are read as UTC.
	if (query.from) query.from = toRfc3339(query.from);
	if (query.to) query.to = toRfc3339(query.to);
	// Grouping by a tag: `tag:<key>`, chosen as such or as "tag" with the key typed next to it.
	for (const name of ['by', 'group_by']) {
		if (query[name] !== 'tag') continue;
		const key = url.searchParams.get('group_tag')?.trim();
		if (key) query[name] = `tag:${key}`;
		else if (defaults[name]) query[name] = defaults[name];
		else delete query[name];
	}
	if (allowed.includes('tag.{key}')) Object.assign(query, tagFilters(url));
	for (const name of FORM_ONLY) {
		const value = url.searchParams.get(name);
		if (value) filters[name] = value;
	}
	for (const [name, value] of url.searchParams) {
		if (name.startsWith('tag.') && value !== '') filters[name] = value;
	}
	return { query, filters };
}

/**
 * The tag keys to offer: those the selected workspace declares, or `null` when no workspace
 * is selected and the key has to be typed.
 */
export async function tagKeysOf(event: KitEvent): Promise<string[] | null> {
	const org = event.url.searchParams.get('organisation_id');
	const ws = event.url.searchParams.get('workspace_id');
	if (!org || !ws) return null;
	const workspace = await event.locals.api.GET('/orgs/{org}/workspaces/{ws}', {
		params: { path: { org, ws } }
	});
	return workspace.data?.tag_keys ?? null;
}

/**
 * Loads a report. Only the named parameters are taken from the page's URL: the analytics
 * service refuses a parameter it does not know. `tag.{key}` in the list admits tag filters.
 * `fixed` parameters are sent whatever the URL says, and are not shown as filters.
 */
export async function loadReport(
	event: KitEvent,
	path: string,
	allowed: string[],
	defaults: Record<string, string> = {},
	fixed: Record<string, string> = {}
): Promise<Report> {
	const { query: asked, filters } = reportQuery(event.url, allowed, defaults);
	const query = { ...asked, ...fixed };
	const result = await loose(event.locals.api).GET(path, { params: { query } });
	if (!result.response.ok)
		return { data: null, problem: problemOf(event, result, allowed), filters };
	const data = result.data;
	return {
		data:
			typeof data === 'object' && data !== null && !Array.isArray(data)
				? (data as Record<string, unknown>)
				: { rows: data },
		problem: null,
		filters
	};
}

/**
 * A report as a CSV file. The call is made here, with the session; the browser gets the
 * file with a name that says what it holds.
 */
export async function csvDownload(
	event: KitEvent,
	path: string,
	allowed: string[],
	defaults: Record<string, string>,
	name: (query: Record<string, string>) => string
): Promise<Response> {
	if (!event.locals.token) error(401, 'Sign in first.');
	const { query } = reportQuery(event.url, allowed, defaults);
	const search = new URLSearchParams({ ...query, format: 'csv' });
	let upstream: Response;
	try {
		upstream = await fetch(`${GATEWAY_API_URL}${API_BASE}${path}?${search}`, {
			headers: { authorization: `Bearer ${event.locals.token}`, accept: 'text/csv' },
			signal: event.request.signal
		});
	} catch {
		error(502, 'The gateway does not answer.');
	}
	if (!upstream.ok) {
		const problem = toProblem(upstream.status, await upstream.json().catch(() => null));
		error(
			upstream.status >= 400 && upstream.status <= 599 ? upstream.status : 502,
			problem.message
		);
	}
	const day = new Date().toISOString().slice(0, 10);
	const file = `pacod-gw-${name(query)}-${day}.csv`.replace(/[^A-Za-z0-9._-]+/g, '-');
	return new Response(upstream.body, {
		headers: {
			'content-type': 'text/csv; charset=utf-8',
			'content-disposition': `attachment; filename="${file}"`
		}
	});
}
