/**
 * Answers of Prometheus as the console shows them: series on a time axis, a headline figure,
 * the rows of a table, and values written in their unit.
 */
import { formatCost, formatInteger, formatMs } from '../format';
import { PALETTE, type ChartSeries } from '../report/stack';
import type { Unit } from './boards';

/** One series of a Prometheus answer: a range query has `values`, an instant query `value`. */
export interface PromResult {
	metric: Record<string, string>;
	values?: [number, string][];
	value?: [number, string];
}

/** A value of Prometheus as a number; `NaN` and the infinities are no value. */
export function numberOf(text: string | undefined): number | null {
	if (text === undefined) return null;
	const value = Number(text);
	return Number.isFinite(value) ? value : null;
}

/** The name of a series: the legend with its labels filled in, or the labels themselves. */
export function legendOf(metric: Record<string, string>, legend?: string): string {
	if (legend !== undefined)
		return legend.includes('{{')
			? legend.replace(/\{\{\s*(\w+)\s*\}\}/g, (_, name: string) => metric[name] || '–')
			: legend;
	const labels = Object.entries(metric).filter(([name]) => name !== '__name__');
	return labels.length > 0 ? labels.map(([name, value]) => `${name}=${value}`).join(', ') : 'value';
}

/** The time axis of a range: every step from start to end. */
export function timeAxis(start: number, end: number, step: number): number[] {
	const axis: number[] = [];
	for (let at = start; at <= end; at += step) axis.push(at);
	return axis;
}

/** `19:30` within a day, `10-05 19:30` beyond; always UTC. */
export function timeLabel(at: number, seconds: number): string {
	const iso = new Date(at * 1000).toISOString();
	return seconds <= 24 * 3600 ? iso.slice(11, 16) : `${iso.slice(5, 10)} ${iso.slice(11, 16)}`;
}

/**
 * The series of one or more range answers on one axis, coloured in order. A step without a
 * value is a gap, not a zero: Prometheus had nothing to say for it.
 */
export function chartSeries(
	answers: { result: PromResult[]; legend?: string }[],
	axis: number[]
): ChartSeries[] {
	const index = new Map(axis.map((at, i) => [at, i]));
	const series: ChartSeries[] = [];
	for (const answer of answers) {
		for (const result of answer.result) {
			const values = new Array<number | null>(axis.length).fill(null);
			for (const [at, text] of result.values ?? []) {
				const i = index.get(at);
				if (i !== undefined) values[i] = numberOf(text);
			}
			series.push({
				name: legendOf(result.metric, answer.legend),
				values,
				colour: PALETTE[series.length % PALETTE.length]
			});
		}
	}
	return series;
}

const COMPACT = new Intl.NumberFormat('en-GB', { notation: 'compact', maximumFractionDigits: 2 });
const DECIMAL = new Intl.NumberFormat('en-GB', { maximumSignificantDigits: 3 });
const PERCENT = new Intl.NumberFormat('en-GB', { style: 'percent', maximumFractionDigits: 1 });

function bytes(value: number): string {
	const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
	let size = value;
	let unit = 0;
	while (Math.abs(size) >= 1024 && unit < units.length - 1) {
		size /= 1024;
		unit++;
	}
	return `${DECIMAL.format(size)} ${units[unit]}`;
}

/** A value in its unit: `1.2/s`, `4.5%`, `850 ms`, `12.4k`, `3 MiB`. */
export function formatUnit(value: number | null, unit: Unit): string {
	if (value === null) return '–';
	switch (unit) {
		case 'reqps':
			return `${DECIMAL.format(value)}/s`;
		case 'percent':
			return PERCENT.format(value);
		case 'seconds':
			return value < 1 ? formatMs(value * 1000) : `${DECIMAL.format(value)} s`;
		case 'money':
			return formatCost(value, '');
		case 'bytes':
			return bytes(value);
		case 'days':
			return `${DECIMAL.format(value)} d`;
		case 'count':
			return Number.isInteger(value) ? formatInteger(value) : DECIMAL.format(value);
		default:
			return Math.abs(value) >= 1000 ? COMPACT.format(value) : DECIMAL.format(value);
	}
}

/**
 * The figure of a headline panel: its one value, or one value per series named by the legend
 * (spend per currency). No series is no value.
 */
export function statValue(result: PromResult[], unit: Unit, legend?: string): string {
	const values = result.map((r) => ({
		name: legend ? legendOf(r.metric, legend) : '',
		value: numberOf(r.value?.[1])
	}));
	if (values.length === 0) return '–';
	return values
		.map(({ name, value }) => `${formatUnit(value, unit)}${name ? ` ${name}` : ''}`)
		.join(' · ');
}

/**
 * The rows of a table panel: one per label set, its labels and a column per query, in the
 * order of the first query's answer. Counts that Prometheus extrapolated are rounded.
 */
export function tableRows(
	answers: { result: PromResult[]; column: string }[],
	labels: string[],
	unit: Unit
): Record<string, string | number | null>[] {
	const rows = new Map<string, Record<string, string | number | null>>();
	for (const answer of answers) {
		for (const result of answer.result) {
			const key = labels.map((name) => result.metric[name] ?? '').join('\u0000');
			const row =
				rows.get(key) ??
				Object.fromEntries(labels.map((name) => [name, result.metric[name] ?? '']));
			const value = numberOf(result.value?.[1]);
			row[answer.column] = value === null || unit === 'money' ? value : Math.round(value);
			rows.set(key, row);
		}
	}
	return [...rows.values()];
}
