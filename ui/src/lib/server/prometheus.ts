/**
 * Prometheus, for the Metrics page: the queries of the Grafana dashboards, asked over the HTTP
 * API from the server side of the console. The browser never talks to Prometheus.
 */
import { PROMETHEUS_URL } from '$app/env/private';
import type { PromResult } from '#lib/metrics/series.ts';

/** A query that takes longer is given up; the panel says so. */
const TIMEOUT_MS = 10_000;

export type PromAnswer =
	{ result: PromResult[]; problem: null } | { result: null; problem: string };

export const prometheusConfigured = (): boolean => Boolean(PROMETHEUS_URL);

async function get(path: string, params: URLSearchParams): Promise<unknown> {
	const response = await fetch(`${PROMETHEUS_URL}${path}?${params}`, {
		signal: AbortSignal.timeout(TIMEOUT_MS)
	});
	const body = (await response.json().catch(() => null)) as {
		status?: string;
		data?: unknown;
		error?: string;
	} | null;
	if (!response.ok || body?.status !== 'success')
		throw new Error(body?.error || `Prometheus answered ${response.status}.`);
	return body.data;
}

function failed(error: unknown): PromAnswer {
	const message =
		error instanceof Error && error.name === 'TimeoutError'
			? 'Prometheus did not answer in time.'
			: error instanceof Error
				? error.message
				: 'Prometheus could not be asked.';
	return { result: null, problem: message };
}

/** A query over a range: one series per label set, a value per step. */
export async function queryRange(
	expr: string,
	start: number,
	end: number,
	step: number
): Promise<PromAnswer> {
	try {
		const data = (await get(
			'/api/v1/query_range',
			new URLSearchParams({ query: expr, start: String(start), end: String(end), step: `${step}s` })
		)) as { result?: PromResult[] };
		return { result: data.result ?? [], problem: null };
	} catch (error) {
		return failed(error);
	}
}

/** A query at one instant: one value per label set. */
export async function queryInstant(expr: string, time: number): Promise<PromAnswer> {
	try {
		const data = (await get(
			'/api/v1/query',
			new URLSearchParams({ query: expr, time: String(time) })
		)) as { result?: PromResult[] };
		return { result: data.result ?? [], problem: null };
	} catch (error) {
		return failed(error);
	}
}

/** The values a label takes in the series a selector matches; none when Prometheus fails. */
export async function labelValues(label: string, match: string, start: number, end: number) {
	try {
		const data = await get(
			`/api/v1/label/${encodeURIComponent(label)}/values`,
			new URLSearchParams({ 'match[]': match, start: String(start), end: String(end) })
		);
		return Array.isArray(data) ? data.filter((v): v is string => typeof v === 'string') : [];
	} catch {
		return [];
	}
}
