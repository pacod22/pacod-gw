/** Time series of a usage report, prepared for a chart and a table. */
import { bucketLabel, numberOf, rowsOf, type Row } from './format';

export interface ChartPoint {
	label: string;
	value: number;
}

/** Every point of every series as one row, with the group it belongs to. */
export function seriesRows(data: unknown): Row[] {
	return rowsOf(data, 'series').flatMap((series) =>
		rowsOf(series, 'points').map((point) => ({ group: series.group ?? '', ...point }))
	);
}

/** One measure per bucket, summed over the groups, in time order. */
export function chartPoints(
	data: unknown,
	measure: string,
	bucket: string,
	combine: 'sum' | 'max' = 'sum'
): ChartPoint[] {
	const byBucket = new Map<string, number>();
	for (const row of seriesRows(data)) {
		const key = String(row.bucket ?? '');
		const value = numberOf(row, measure);
		const before = byBucket.get(key);
		byBucket.set(
			key,
			before === undefined ? value : combine === 'sum' ? before + value : Math.max(before, value)
		);
	}
	return [...byBucket.entries()]
		.sort(([a], [b]) => a.localeCompare(b))
		.map(([key, value]) => ({ label: bucketLabel(key, bucket), value }));
}
