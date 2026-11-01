/**
 * The review queue: everything about the model repository that waits for a platform admin, in
 * the order a new model goes through it. Discovery finds a model, it is added as a draft, the
 * price tracker proposes its first price, the price is approved, the model is activated. Price
 * changes of models in use and models the providers no longer list wait here too.
 */

/** Where an item stands. The order is the order of the steps. */
export type Step = 'new' | 'needs_price' | 'price_proposed' | 'ready' | 'price_change' | 'gone';

export const STEPS: readonly Step[] = [
	'new',
	'needs_price',
	'price_proposed',
	'ready',
	'price_change',
	'gone'
];

/** What each step asks for, as a heading and as the column of a row. */
export const STEP_LABELS: Record<Step, string> = {
	new: 'New at the provider',
	needs_price: 'Draft without a price',
	price_proposed: 'First price to approve',
	ready: 'Ready to activate',
	price_change: 'Price change to approve',
	gone: 'Gone from the provider'
};

/** A proposal of model discovery, as `GET /discovery/proposals` gives it. */
export interface DiscoveryProposal {
	id: string;
	provider_id: string;
	provider: string;
	upstream_id: string;
	kind: string;
	model_id?: string | null;
	details?: unknown;
	first_seen: string;
}

/** A price waiting for approval, as `GET /price-proposals` gives it. */
export interface PriceProposal {
	id: string;
	model_id: string;
	model: string;
	effective_from?: string | null;
}

/** A model of the repository, as `GET /models` gives it. */
export interface RepoModel {
	id: string;
	name: string;
	provider_id: string;
	provider: string;
	upstream_id: string;
	state: string;
	priced: boolean;
	free: boolean;
	created_at?: string | null;
}

/** One thing to decide or do. */
export interface ReviewItem<P extends PriceProposal = PriceProposal> {
	/** Unique within the queue. */
	key: string;
	step: Step;
	provider_id: string;
	provider: string;
	/** The repository's name of the model, or the provider's id for a model not yet in it. */
	name: string;
	upstream_id: string;
	/** The repository model, where there is one. */
	model_id: string | null;
	/** The discovery proposal, for `new` and `gone`. */
	proposal_id: string | null;
	/** The price proposal, for `price_proposed` and `price_change`. */
	price: P | null;
	/** For `needs_price`: whether the price tracker has a source for the provider. */
	tracked: boolean;
	/** Since when the item waits, where that is known. */
	since: string | null;
}

export interface ReviewGroup<P extends PriceProposal = PriceProposal> {
	provider_id: string;
	provider: string;
	items: ReviewItem<P>[];
}

/**
 * The queue, from what the management API lists. `tracked` holds the providers the price
 * tracker covers: none while the job is switched off.
 */
export function reviewItems<P extends PriceProposal>(
	proposals: DiscoveryProposal[],
	prices: P[],
	models: RepoModel[],
	tracked: ReadonlySet<string>
): ReviewItem<P>[] {
	const byId = new Map(models.map((m) => [m.id, m]));
	const items: ReviewItem<P>[] = [];
	const base = { model_id: null, proposal_id: null, price: null, tracked: false, since: null };

	for (const p of proposals) {
		if (p.kind !== 'new' && p.kind !== 'gone') continue;
		const model = p.model_id ? byId.get(p.model_id) : undefined;
		items.push({
			...base,
			key: `proposal:${p.id}`,
			step: p.kind,
			// The core sends no name for a provider that was deleted since.
			provider_id: p.provider_id,
			provider: p.provider || p.provider_id,
			name: model?.name ?? p.upstream_id.replace(/^models\//, ''),
			upstream_id: p.upstream_id,
			model_id: model?.id ?? p.model_id ?? null,
			proposal_id: p.id,
			since: p.first_seen
		});
	}

	// A draft whose first price waits for approval is at that step, not at "without a price". A
	// draft that has a price already is ready, and its proposal is a change like any other.
	const waiting = new Set<string>();
	for (const price of prices) {
		const model = byId.get(price.model_id);
		const first = model?.state === 'draft' && !model.priced && !model.free;
		if (first) waiting.add(price.model_id);
		items.push({
			...base,
			key: `price:${price.id}`,
			step: first ? 'price_proposed' : 'price_change',
			provider_id: model?.provider_id ?? '',
			provider: model?.provider ?? '',
			name: model?.name ?? price.model,
			upstream_id: model?.upstream_id ?? '',
			model_id: price.model_id,
			price,
			since: price.effective_from ?? null
		});
	}

	for (const m of models) {
		if (m.state !== 'draft' || waiting.has(m.id)) continue;
		const ready = m.priced || m.free;
		items.push({
			...base,
			key: `model:${m.id}`,
			step: ready ? 'ready' : 'needs_price',
			provider_id: m.provider_id,
			provider: m.provider || m.provider_id,
			name: m.name,
			upstream_id: m.upstream_id,
			model_id: m.id,
			tracked: !ready && tracked.has(m.provider_id),
			since: m.created_at ?? null
		});
	}
	return items;
}

/** Items grouped by provider, providers by name, items by step and then by name. */
export function byProvider<P extends PriceProposal>(items: ReviewItem<P>[]): ReviewGroup<P>[] {
	const groups = new Map<string, ReviewGroup<P>>();
	for (const item of items) {
		let group = groups.get(item.provider_id);
		if (!group) {
			group = { provider_id: item.provider_id, provider: item.provider, items: [] };
			groups.set(item.provider_id, group);
		}
		group.items.push(item);
	}
	const order = (s: Step) => STEPS.indexOf(s);
	for (const group of groups.values())
		group.items.sort(
			(a, b) =>
				order(a.step) - order(b.step) || a.name.localeCompare(b.name, 'en', { numeric: true })
		);
	return [...groups.values()].sort((a, b) =>
		a.provider.localeCompare(b.provider, 'en', { numeric: true })
	);
}

/** How many items wait at each step. */
export function stepCounts(items: ReviewItem[]): Record<Step, number> {
	const counts = Object.fromEntries(STEPS.map((s) => [s, 0])) as Record<Step, number>;
	for (const item of items) counts[item.step] += 1;
	return counts;
}

/** The providers the price tracker covers: those its sources name, while it is switched on. */
export function trackedProviders(
	job: { enabled?: boolean; sources?: { provider_id: string }[] } | null | undefined
): Set<string> {
	if (!job?.enabled) return new Set();
	return new Set((job.sources ?? []).map((s) => s.provider_id));
}
