/**
 * Benchmark reports (pacod-bench-report/1) as the Benchmarks pages show them: one row a cell
 * with its overhead against its baseline, the series of the charts, and reports side by side.
 * Pure, so that it is tested without SvelteKit.
 *
 * The overhead is a cell's latency minus its baseline's (the same load straight to the mock).
 * It is what any gateway adds, this one or another product, and it includes what the gateway's
 * own figure (`x-gw-delay-ms`) leaves out: relaying a stream. Without a baseline it is absent,
 * never 0, and so is the gateway's own figure for another product.
 */
import type { Schemas } from '#lib/api/types.ts';
import type { ChartSeries } from '#lib/report/stack.ts';

export type BenchmarkReport = Schemas['Report'];
export type BenchmarkSummary = Schemas['BenchmarkSummary'];
export type ReportCell = Schemas['Cell'];
export type ReportSummary = Schemas['Summary'];

export interface CellRow {
	cell: ReportCell;
	steady: ReportSummary | null;
	/** Whole call minus the baseline's, at p50 and p99, in ms; `null` without a baseline. */
	overheadP50: number | null;
	overheadP99: number | null;
}

/** Every cell of a report, a target cell measured against the baseline it names. */
export function cellRows(report: BenchmarkReport): CellRow[] {
	const baselines = new Map<string, ReportSummary>();
	for (const c of report.cells) {
		if (c.kind === 'baseline' && c.status === 'done' && c.steady)
			baselines.set(c.comparison_key, c.steady);
	}
	return report.cells.map((cell) => {
		const steady = cell.steady ?? null;
		const base =
			cell.kind === 'target' && cell.baseline_key ? baselines.get(cell.baseline_key) : undefined;
		const ok = steady && steady.ok > 0 && base && base.ok > 0;
		return {
			cell,
			steady,
			overheadP50: ok ? steady.total.p50 - base.total.p50 : null,
			overheadP99: ok ? steady.total.p99 - base.total.p99 : null
		};
	});
}

/** Latency of each finished target cell as grouped columns: p50, p95, p99. */
export function latencyBars(
	report: BenchmarkReport,
	key: 'total' | 'ttfb' | 'gw_delay'
): { labels: string[]; series: ChartSeries[] } {
	const done = report.cells.filter((c) => c.kind === 'target' && c.status === 'done' && c.steady);
	const pick = (c: ReportCell, q: 'p50' | 'p95' | 'p99') => {
		const s = c.steady;
		const p = s ? (key === 'gw_delay' ? s.gw_delay : s[key]) : null;
		return p ? p[q] : null;
	};
	return {
		labels: done.map((c) => c.label),
		series: [
			{ name: 'p50', values: done.map((c) => pick(c, 'p50')), colour: '--series-1', bar: true },
			{ name: 'p95', values: done.map((c) => pick(c, 'p95')), colour: '--series-3', bar: true },
			{ name: 'p99', values: done.map((c) => pick(c, 'p99')), colour: '--series-5', bar: true }
		]
	};
}

export function overheadBars(report: BenchmarkReport): { labels: string[]; series: ChartSeries[] } {
	const rows = cellRows(report).filter((r) => r.cell.kind === 'target' && r.cell.status === 'done');
	return {
		labels: rows.map((r) => r.cell.label),
		series: [
			{ name: 'p50', values: rows.map((r) => r.overheadP50), colour: '--series-1', bar: true },
			{ name: 'p99', values: rows.map((r) => r.overheadP99), colour: '--series-5', bar: true }
		]
	};
}

/** What the environment's facts are called on the page. */
export const FACT_LABELS: Record<string, string> = {
	gateway_version: 'Gateway version',
	instances: 'Instances',
	cache_store: 'Cache store',
	analytics: 'Analytics',
	layout: 'Layout',
	arch: 'CPU architecture',
	cpus: 'CPUs',
	memory_gb: 'Memory (GB)',
	mock: 'Mock',
	notes: 'Notes'
};

const text = (v: unknown): string =>
	v === undefined || v === null || v === '' ? '–' : typeof v === 'string' ? v : JSON.stringify(v);

export interface FactRow {
	name: string;
	label: string;
	values: string[];
	sources: string[];
	differs: boolean;
}

/** The environment of each report, fact by fact, the known ones first. */
export function factRows(reports: BenchmarkReport[]): FactRow[] {
	const names = new Set(reports.flatMap((r) => Object.keys(r.environment ?? {})));
	const known = Object.keys(FACT_LABELS).filter((k) => names.has(k));
	const rest = [...names].filter((k) => !(k in FACT_LABELS)).sort();
	return [...known, ...rest].map((name) => {
		const values = reports.map((r) => text(r.environment?.[name]?.value));
		return {
			name,
			label: FACT_LABELS[name] ?? name,
			values,
			sources: reports.map((r) => r.environment?.[name]?.source ?? ''),
			differs: new Set(values).size > 1
		};
	});
}

export interface AlignedRow {
	key: string;
	label: string;
	cells: (CellRow | null)[];
}

/** Target cells that ran the same configuration in at least two reports, side by side. */
export function alignReports(reports: BenchmarkReport[]): AlignedRow[] {
	const rows = new Map<string, AlignedRow>();
	reports.forEach((report, i) => {
		for (const row of cellRows(report)) {
			if (row.cell.kind !== 'target') continue;
			const key = row.cell.comparison_key;
			const aligned = rows.get(key) ?? {
				key,
				label: row.cell.label,
				cells: reports.map(() => null)
			};
			// Repetitions of one configuration: the first that ran stands for them.
			if (!aligned.cells[i] || aligned.cells[i]!.cell.status !== 'done') aligned.cells[i] = row;
			rows.set(key, aligned);
		}
	});
	return [...rows.values()].filter(
		(r) => r.cells.filter((c) => c?.cell.status === 'done').length >= 2
	);
}

/** Milliseconds as the pages write them; `n/a` for an absent figure. */
export function ms(value: number | null | undefined): string {
	if (value === null || value === undefined || !Number.isFinite(value)) return 'n/a';
	if (Math.abs(value) >= 1000) return `${(value / 1000).toFixed(2)} s`;
	if (Math.abs(value) >= 100) return `${value.toFixed(0)} ms`;
	if (Math.abs(value) >= 10) return `${value.toFixed(1)} ms`;
	return `${value.toFixed(2)} ms`;
}

/** A difference of milliseconds, with its sign. */
export function deltaMs(value: number | null | undefined): string {
	if (value === null || value === undefined || !Number.isFinite(value)) return 'n/a';
	return `${value >= 0 ? '+' : '−'}${ms(Math.abs(value))}`;
}
