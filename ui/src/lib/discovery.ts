/**
 * Model discovery (concept section 6.4) as the console shows it: proposals grouped by
 * provider, what each provider reported about a model, and the names a draft can take.
 */

/** What a provider reported about a model it lists, where it reports it. */
export interface Reported {
	display_name: string | null;
	context_window: number | null;
	max_output_tokens: number | null;
	vision: boolean | null;
	reasons: boolean | null;
}

/**
 * What a proposal's details say. A display name that only repeats the provider's id (as
 * Gemini's list does) is left out: it tells nothing the id does not.
 */
export function reportedOf(upstreamId: string, details: unknown): Reported {
	const d = (details && typeof details === 'object' ? details : {}) as Record<string, unknown>;
	const number = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
	const flag = (v: unknown) => (typeof v === 'boolean' ? v : null);
	const name = typeof d.display_name === 'string' ? d.display_name.trim() : '';
	return {
		display_name: name && name !== bareId(upstreamId) ? name : null,
		context_window: number(d.context_window),
		max_output_tokens: number(d.max_output_tokens),
		vision: flag(d.vision),
		reasons: flag(d.reasons)
	};
}

/** What the provider says the model takes beyond text: "images, reasoning". */
export function takesOf(reported: Reported): string {
	return [reported.vision === true && 'images', reported.reasons === true && 'reasoning']
		.filter(Boolean)
		.join(', ');
}

/** Gemini's own list names models `models/<id>`; the core drops the prefix in a name. */
function bareId(upstreamId: string): string {
	return upstreamId.replace(/^models\//, '');
}

/**
 * The two names the core gives an accepted model when none is typed. Mirrors `Naming` in
 * `crates/pacod-gw-core/src/mgmt/discovery.rs`.
 */
export function draftNames(
	provider: string,
	upstreamId: string
): { prefixed: string; bare: string } {
	const bare = bareId(upstreamId);
	return { prefixed: `${provider}/${bare}`, bare };
}

export interface ProviderGroup<T> {
	provider_id: string;
	provider: string;
	rows: T[];
}

/** Rows grouped by provider, providers by name, rows in the order they came. */
export function byProvider<T extends { provider_id: string; provider: string }>(
	rows: T[]
): ProviderGroup<T>[] {
	const groups = new Map<string, ProviderGroup<T>>();
	for (const row of rows) {
		let group = groups.get(row.provider_id);
		if (!group) {
			group = { provider_id: row.provider_id, provider: row.provider, rows: [] };
			groups.set(row.provider_id, group);
		}
		group.rows.push(row);
	}
	return [...groups.values()].sort((a, b) =>
		a.provider.localeCompare(b.provider, 'en', { numeric: true })
	);
}

/** Whether a proposal matches the filter: by the provider's id, its name for it, or the provider. */
export function matchesQuery(
	row: { upstream_id: string; provider: string; display_name?: string | null },
	query: string
): boolean {
	const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
	const text = [row.upstream_id, row.provider, row.display_name ?? ''].join(' ').toLowerCase();
	return words.every((word) => text.includes(word));
}

/** What a run found at one provider, or why it could not ask. */
export interface ProviderResult {
	provider: string;
	listed: number | null;
	error: string | null;
}

/** The per-provider part of a discovery run's `result`, by provider name. */
export function runResults(result: unknown): ProviderResult[] {
	if (!result || typeof result !== 'object') return [];
	return Object.entries(result as Record<string, unknown>)
		.map(([provider, value]) => {
			const v = (value && typeof value === 'object' ? value : {}) as Record<string, unknown>;
			return {
				provider,
				listed: typeof v.listed === 'number' ? v.listed : null,
				error: typeof v.error === 'string' ? v.error : null
			};
		})
		.sort((a, b) => a.provider.localeCompare(b.provider, 'en', { numeric: true }));
}

/**
 * How long the page waits for a run that has not finished. A run asks the providers one after
 * another, each within its timeout; one that is still open after this broke off (its instance
 * stopped, or the run failed before it could record its end).
 */
const UNFINISHED_AFTER_MS = 60 * 60 * 1000;

export type RunState = 'running' | 'unfinished' | 'finished' | 'failed';

/** What became of a run. */
export function runState(
	run: { started_at: string; finished_at?: string | null; ok?: boolean | null },
	now: Date
): RunState {
	if (!run.finished_at) {
		const started = Date.parse(run.started_at);
		return now.getTime() - started > UNFINISHED_AFTER_MS ? 'unfinished' : 'running';
	}
	return run.ok === false ? 'failed' : 'finished';
}

/**
 * When the schedule runs discovery next: the last start plus the interval, `due` when that
 * has passed (a management instance starts it within half a minute), `null` when it runs only
 * when asked.
 */
export function nextRun(
	lastStarted: string | null | undefined,
	intervalHours: number | null | undefined,
	now: Date
): string | 'due' | null {
	if (!intervalHours || intervalHours <= 0) return null;
	const last = lastStarted ? Date.parse(lastStarted) : Number.NaN;
	if (Number.isNaN(last)) return 'due';
	const next = last + intervalHours * 3_600_000;
	return next <= now.getTime() ? 'due' : new Date(next).toISOString();
}

export interface PresetGroup<P> {
	family: string;
	presets: P[];
}

/** Capability presets by provider family, for a select with one group per family. */
export function presetsByFamily<P extends { family: string }>(presets: P[]): PresetGroup<P>[] {
	const groups = new Map<string, P[]>();
	for (const preset of presets)
		groups.set(preset.family, [...(groups.get(preset.family) ?? []), preset]);
	return [...groups.entries()]
		.sort(([a], [b]) => a.localeCompare(b))
		.map(([family, list]) => ({ family, presets: list }));
}
