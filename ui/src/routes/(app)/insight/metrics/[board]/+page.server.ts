import { error } from '@sveltejs/kit';
import { hasPlatformRole } from '#lib/access.ts';
import {
	boardOf,
	expand,
	metricRangeOf,
	stepsOf,
	type Filter,
	type Panel
} from '#lib/metrics/boards.ts';
import { chartSeries, statValue, tableRows, timeAxis, timeLabel } from '#lib/metrics/series.ts';
import type { ChartSeries } from '#lib/report/stack.ts';
import {
	labelValues,
	prometheusConfigured,
	queryInstant,
	queryRange
} from '#lib/server/prometheus.ts';
import type { PageServerLoad } from './$types';

/** What the page draws for a panel: a figure, series over time or the rows of a table. */
export interface PanelView {
	kind: Panel['kind'];
	title: string;
	description: string;
	unit: Panel['unit'];
	stacked: boolean;
	problem: string | null;
	value?: string;
	series?: ChartSeries[];
	columns?: string[];
	rows?: Record<string, string | number | null>[];
}

/** The query parameter of each filter; the scope is the one the rest of the console uses. */
const PARAMS: Record<Filter, string> = {
	organisation: 'organisation_id',
	workspace: 'workspace_id',
	provider: 'provider',
	model: 'model',
	instance: 'instance'
};

export const load: PageServerLoad = async (event) => {
	const principal = event.locals.principal;
	// The metrics count every organisation alike: they are for the platform's operators.
	if (!principal || !hasPlatformRole(principal))
		error(403, 'The metrics cover every organisation; they are shown to platform roles only.');
	const board = boardOf(event.params.board);
	if (!board) error(404, 'There is no such metrics board.');
	event.depends('app:metrics');

	const range = metricRangeOf(event.url.searchParams.get('range'));
	const values: Record<string, string> = { range: range.id };
	const filters: Partial<Record<Filter, string>> = {};
	for (const name of board.filters) {
		const value = event.url.searchParams.get(PARAMS[name]) ?? '';
		if (value) {
			filters[name] = value;
			values[PARAMS[name]] = value;
		}
	}
	const base = { board: board.id, range: range.id, values };
	if (!prometheusConfigured())
		return { ...base, configured: false, updated: null, labels: [], sections: [], choices: {} };

	const { step, rate } = stepsOf(range.seconds);
	const end = Math.floor(Date.now() / 1000 / step) * step;
	const start = end - Math.floor(range.seconds / step) * step;
	const axis = timeAxis(start, end, step);
	const promql = (expr: string) => expand(expr, filters, rate, range.seconds);

	const view = async (panel: Panel): Promise<PanelView> => {
		const head = {
			kind: panel.kind,
			title: panel.title,
			description: panel.description,
			unit: panel.unit,
			stacked: panel.stacked ?? false
		};
		if (panel.kind === 'line') {
			const answers = await Promise.all(
				panel.queries.map((q) => queryRange(promql(q.expr), start, end, step))
			);
			const problem = answers.find((a) => a.problem)?.problem ?? null;
			if (problem) return { ...head, problem };
			const series = chartSeries(
				answers.map((a, i) => ({ result: a.result ?? [], legend: panel.queries[i].legend })),
				axis
			);
			return { ...head, problem: null, series };
		}
		const answers = await Promise.all(panel.queries.map((q) => queryInstant(promql(q.expr), end)));
		const problem = answers.find((a) => a.problem)?.problem ?? null;
		if (problem) return { ...head, problem };
		if (panel.kind === 'stat')
			return {
				...head,
				problem: null,
				value: statValue(answers[0].result ?? [], panel.unit, panel.queries[0].legend)
			};
		const labels = panel.labels ?? [];
		const columns = panel.queries.map((q) => q.column ?? 'value');
		return {
			...head,
			problem: null,
			columns: [...labels, ...columns],
			rows: tableRows(
				answers.map((a, i) => ({ result: a.result ?? [], column: columns[i] })),
				labels,
				panel.unit
			)
		};
	};

	// The values to choose from: those Prometheus has seen in the range.
	const providerMatch = `gw_requests_total{provider=~"${filters.provider ? promql('$provider') : '.*'}"}`;
	const [sections, providers, models, instances] = await Promise.all([
		Promise.all(
			board.sections.map(async (section) => ({
				title: section.title ?? null,
				panels: await Promise.all(section.panels.map(view))
			}))
		),
		board.filters.includes('provider')
			? labelValues('provider', 'gw_requests_total', start, end)
			: [],
		board.filters.includes('model') ? labelValues('model', providerMatch, start, end) : [],
		board.filters.includes('instance')
			? labelValues('instance', 'gw_config_version', start, end)
			: []
	]);

	return {
		...base,
		configured: true,
		updated: new Date().toISOString(),
		labels: axis.map((at) => timeLabel(at, range.seconds)),
		sections,
		choices: { provider: providers, model: models, instance: instances }
	};
};
