/** The gateway's models as the Testing pages offer them. */
import type { KitEvent } from '#lib/server/resource.ts';
import { slotOf, type Slot } from '#lib/testing.ts';

export interface ModelChoice {
	id: string;
	name: string;
	provider: string;
}

async function activeWithProviders(event: KitEvent) {
	const [models, providers] = await Promise.all([
		event.locals.api.GET('/models'),
		event.locals.api.GET('/providers', { params: { query: { limit: 500 } } })
	]);
	const byId = new Map((providers.data?.items ?? []).map((p) => [p.id, p]));
	return (models.data?.items ?? [])
		.filter((m) => m.state === 'active')
		.map((m) => ({ model: m, provider: byId.get(m.provider_id) }));
}

/** The active models, by the tester's slot their provider fills: the choices for live mode. */
export async function liveModels(event: KitEvent) {
	const bySlot: Record<Slot, ModelChoice[]> = { openai: [], anthropic: [], gemini: [] };
	for (const { model, provider } of await activeWithProviders(event)) {
		const s = provider
			? slotOf(provider.family as string, provider.managed_api ?? undefined)
			: null;
		if (!s) continue;
		bySlot[s].push({ id: model.id, name: model.name, provider: model.provider ?? '' });
	}
	for (const list of Object.values(bySlot)) list.sort((a, b) => a.name.localeCompare(b.name));
	return bySlot;
}

/** Every active model, chat and embedding alike: the choices for a live load test. */
export async function activeModels(event: KitEvent): Promise<ModelChoice[]> {
	return (await activeWithProviders(event))
		.map(({ model }) => ({ id: model.id, name: model.name, provider: model.provider ?? '' }))
		.sort((a, b) => a.name.localeCompare(b.name));
}
