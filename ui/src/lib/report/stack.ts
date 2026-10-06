/**
 * A usage report as a stacked chart: one series per group over a continuous time axis, and
 * the share of the cost per group. The analytics service leaves out buckets without calls;
 * here they are put back, so a quiet hour shows as a gap and not as a missing column.
 */
import { toDateTimeInput } from '../schema/form';
import { bucketLabel, numberOf, rowsOf, type Row } from './format';

export type Measure = 'cost' | 'tokens' | 'requests';

/** One series of a chart over time; `colour` is a colour variable of the theme. */
export interface ChartSeries {
	name: string;
	/** `null` is a gap: nothing was measured at that point. */
	values: (number | null)[];
	colour: string;
	/** A column among lines. */
	bar?: boolean;
}

export interface Stack {
	labels: string[];
	series: ChartSeries[];
}

/** The colours of the groups of a stacked chart, in order. */
export const PALETTE = Array.from({ length: 8 }, (_, i) => `--series-${i + 1}`);

export interface Share {
	name: string;
	cost: number;
	share: number;
}

/** More columns than this are not filled in: the chart would be unreadable anyway. */
const MAX_BUCKETS = 1000;
const HOUR = 3_600_000;
const DAY = 24 * HOUR;

/** The name of a group without a value: calls that never reached a provider, for example. */
export const NO_GROUP = '(none)';
export const OTHER = 'Other';

/** How far back the quick switch of a bucket reaches when no range is chosen, as in PACOD. */
export function bucketSpan(bucket: string): number {
	switch (bucket) {
		case 'day':
			return 30 * DAY;
		case 'week':
			return 12 * 7 * DAY;
		case 'month':
			return 365 * DAY;
		default:
			return 48 * HOUR;
	}
}

/** The start of the bucket an instant falls in, in UTC; weeks start on Monday. */
export function bucketStart(at: number, bucket: string): number {
	const d = new Date(at);
	switch (bucket) {
		case 'day':
			return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate());
		case 'week': {
			const day = Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate());
			return day - ((d.getUTCDay() + 6) % 7) * DAY;
		}
		case 'month':
			return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1);
		default:
			return Math.floor(at / HOUR) * HOUR;
	}
}

function nextBucket(at: number, bucket: string): number {
	switch (bucket) {
		case 'day':
			return at + DAY;
		case 'week':
			return at + 7 * DAY;
		case 'month': {
			const d = new Date(at);
			return Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1);
		}
		default:
			return at + HOUR;
	}
}

/**
 * Every bucket start in `[from, to)` as epoch milliseconds, the first one being the bucket
 * `from` falls in. `null` when the range is unknown or has too many buckets.
 */
export function bucketSequence(from: unknown, to: unknown, bucket: string): number[] | null {
	const start = typeof from === 'string' ? Date.parse(from) : NaN;
	const end = typeof to === 'string' ? Date.parse(to) : NaN;
	if (Number.isNaN(start) || Number.isNaN(end) || end <= start) return null;
	const out: number[] = [];
	for (let at = bucketStart(start, bucket); at < end; at = nextBucket(at, bucket)) {
		if (out.length === MAX_BUCKETS) return null;
		out.push(at);
	}
	return out;
}

function measureOf(point: Row, measure: Measure): number {
	return measure === 'tokens'
		? numberOf(point, 'input_tokens') + numberOf(point, 'output_tokens')
		: numberOf(point, measure);
}

function nameOf(group: unknown): string {
	return typeof group === 'string' && group !== '' ? group : NO_GROUP;
}

interface TimedPoint {
	at: number;
	point: Row;
}

function timed(points: Row[]): TimedPoint[] {
	return points
		.map((point) => ({ at: Date.parse(String(point.bucket ?? '')), point }))
		.filter((p) => !Number.isNaN(p.at));
}

/**
 * The time axis of a report: every bucket of its range when the range is known, otherwise
 * the buckets that were seen. A point outside the range is kept on the axis, not lost.
 */
function timeAxis(report: Row, seen: number[]): { axis: number[]; index: Map<number, number> } {
	const bucket = typeof report.bucket === 'string' ? report.bucket : 'hour';
	const range = bucketSequence(report.from, report.to, bucket) ?? [];
	const axis = [...new Set([...range, ...seen])].sort((a, b) => a - b);
	return { axis, index: new Map(axis.map((at, i) => [at, i])) };
}

function axisLabels(report: Row, axis: number[]): string[] {
	const bucket = typeof report.bucket === 'string' ? report.bucket : 'hour';
	return axis.map((at) => bucketLabel(new Date(at).toISOString(), bucket));
}

function reportOf(data: unknown): Row {
	return (typeof data === 'object' && data !== null ? data : {}) as Row;
}

/**
 * One series per group of a usage report, the largest by cost first and the rest folded into
 * "Other". The order follows the cost whatever is measured, so a group keeps its colour when
 * the measure changes.
 */
export function stackSeries(data: unknown, measure: Measure, top = 8): Stack {
	const report = reportOf(data);
	const groups = rowsOf(report, 'series').map((series) => {
		const points = timed(rowsOf(series, 'points'));
		return {
			name: nameOf(series.group),
			cost: points.reduce((sum, p) => sum + numberOf(p.point, 'cost'), 0),
			points
		};
	});
	if (groups.every((g) => g.points.length === 0)) return { labels: [], series: [] };
	const { axis, index } = timeAxis(
		report,
		groups.flatMap((g) => g.points.map((p) => p.at))
	);

	const ranked = [...groups].sort((a, b) => b.cost - a.cost || a.name.localeCompare(b.name));
	const shown = ranked.slice(0, top);
	const rest = ranked.slice(top);
	const valuesOf = (members: typeof groups) => {
		const values = new Array<number>(axis.length).fill(0);
		for (const group of members)
			for (const { at, point } of group.points) values[index.get(at)!] += measureOf(point, measure);
		return values;
	};
	const series: ChartSeries[] = shown.map((g, i) => ({
		name: g.name,
		values: valuesOf([g]),
		colour: PALETTE[i % PALETTE.length]
	}));
	if (rest.length > 0)
		series.push({ name: OTHER, values: valuesOf(rest), colour: '--series-other' });
	return { labels: axisLabels(report, axis), series };
}

/** A measure to draw over time: the field of a point, its name and its colour variable. */
export interface MeasureLine {
	field: string;
	name: string;
	colour: string;
	bar?: boolean;
}

/**
 * Measures of a report over its whole range, summed over the groups: the points are those of
 * every series (a usage report), or the rows under `key` (`over_time` of the errors report).
 */
export function measureSeries(
	data: unknown,
	lines: MeasureLine[],
	key?: string
): { labels: string[]; series: ChartSeries[] } {
	const report = reportOf(data);
	const points = timed(
		key
			? rowsOf(report, key)
			: rowsOf(report, 'series').flatMap((series) => rowsOf(series, 'points'))
	);
	if (points.length === 0) return { labels: [], series: [] };
	const { axis, index } = timeAxis(
		report,
		points.map((p) => p.at)
	);
	const series = lines.map((line) => {
		const values = new Array<number>(axis.length).fill(0);
		for (const { at, point } of points) values[index.get(at)!] += numberOf(point, line.field);
		return { name: line.name, values, colour: line.colour, ...(line.bar ? { bar: true } : {}) };
	});
	return { labels: axisLabels(report, axis), series };
}

/** The cost per group of a breakdown as shares of the whole, the rest folded into "Other". */
export function shareRows(rows: Row[], top = 6): Share[] {
	const all = rows.map((row) => ({ name: nameOf(row.group), cost: numberOf(row, 'cost') }));
	const total = all.reduce((sum, r) => sum + r.cost, 0);
	if (total <= 0) return [];
	const ranked = [...all].sort((a, b) => b.cost - a.cost || a.name.localeCompare(b.name));
	const shown = ranked.slice(0, top);
	const rest = ranked.slice(top).reduce((sum, r) => sum + r.cost, 0);
	if (rest > 0) shown.push({ name: OTHER, cost: rest });
	return shown.map((r) => ({ ...r, share: r.cost / total }));
}

/**
 * The query of a page per bucket over the span that suits it, ending where the shown range
 * ends (`to`, as the report gives it back). The other parameters are kept.
 */
export function bucketSearch(search: string, to: unknown, bucket: string): string {
	const params = new URLSearchParams(search);
	params.set('bucket', bucket);
	const end = typeof to === 'string' ? Date.parse(to) : NaN;
	if (!Number.isNaN(end))
		params.set('from', toDateTimeInput(new Date(end - bucketSpan(bucket)).toISOString()));
	return `?${params}`;
}

/** The breakdown page by a dimension, with the filters of a usage page. */
export function breakdownHref(search: string, by: string): string {
	const params = new URLSearchParams(search);
	for (const name of ['bucket', 'group_by', 'group_tag']) params.delete(name);
	params.set('by', by);
	return `/insight/breakdown?${params}`;
}
