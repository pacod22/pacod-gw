/**
 * Choices for form fields that refer to other objects, so that a form offers what exists
 * instead of asking for an id to be typed.
 */
import type { Option } from '#lib/schema/form.ts';
import type { KitEvent } from './resource.ts';

const LIMIT = 500;
/** Listing the workspaces of every organisation costs one call each: stop offering them beyond this. */
const MAX_ORGS_FOR_WORKSPACES = 25;

/** Users and admin tokens, by id: who a role can be given to. */
export async function principalOptions(event: KitEvent): Promise<Option[]> {
	const [users, tokens] = await Promise.all([
		event.locals.api.GET('/users', { params: { query: { limit: LIMIT } } }),
		event.locals.api.GET('/admin-tokens', { params: { query: { limit: LIMIT } } })
	]);
	return [
		...(users.data?.items ?? []).map((u) => ({
			value: u.id,
			label:
				u.display_name && u.display_name !== u.email ? `${u.email} (${u.display_name})` : u.email,
			help: ''
		})),
		...(tokens.data?.items ?? []).map((t) => ({
			value: t.id,
			label: `Admin token: ${t.name}`,
			help: ''
		}))
	];
}

export async function organisationOptions(event: KitEvent): Promise<Option[]> {
	const orgs = await event.locals.api.GET('/orgs', { params: { query: { limit: LIMIT } } });
	return (orgs.data?.items ?? []).map((o) => ({
		value: o.id,
		label: o.display_name ? `${o.display_name} (${o.name})` : o.name,
		help: ''
	}));
}

/**
 * The workspaces of all organisations. Empty when there are too many organisations to ask
 * each for its workspaces: the form then takes the workspace id as text.
 */
export async function workspaceOptions(event: KitEvent): Promise<Option[]> {
	const orgs = await event.locals.api.GET('/orgs', { params: { query: { limit: LIMIT } } });
	const list = orgs.data?.items ?? [];
	if (list.length === 0 || list.length > MAX_ORGS_FOR_WORKSPACES) return [];
	const perOrg = await Promise.all(
		list.map(async (org) => {
			const result = await event.locals.api.GET('/orgs/{org}/workspaces', {
				params: { path: { org: org.id }, query: { limit: LIMIT } }
			});
			return (result.data?.items ?? []).map((ws) => ({
				value: ws.id,
				label: `${org.name} / ${ws.name}`,
				help: ''
			}));
		})
	);
	return perOrg.flat();
}

export interface ScopeNames {
	organisations: { id: string; name: string }[];
	/** `null` when there are too many organisations to ask each for its workspaces. */
	workspaces:
		{ id: string; name: string; organisationId: string; organisationName: string }[] | null;
}

/** Organisations and workspaces with the names people know them by. */
export async function scopeNames(event: KitEvent): Promise<ScopeNames> {
	const orgs = await event.locals.api.GET('/orgs', { params: { query: { limit: LIMIT } } });
	const organisations = (orgs.data?.items ?? []).map((o) => ({
		id: o.id,
		name: o.display_name || o.name
	}));
	if (organisations.length > MAX_ORGS_FOR_WORKSPACES) return { organisations, workspaces: null };
	const perOrg = await Promise.all(
		organisations.map(async (org) => {
			const result = await event.locals.api.GET('/orgs/{org}/workspaces', {
				params: { path: { org: org.id }, query: { limit: LIMIT } }
			});
			return (result.data?.items ?? []).map((ws) => ({
				id: ws.id,
				name: ws.display_name || ws.name,
				organisationId: org.id,
				organisationName: org.name
			}));
		})
	);
	return { organisations, workspaces: perOrg.flat() };
}

/** The IANA time zones this runtime knows, UTC first. */
export function timeZoneOptions(): Option[] {
	const zones = Intl.supportedValuesOf('timeZone').filter((z) => z !== 'UTC');
	return ['UTC', ...zones].map((zone) => ({ value: zone, label: zone, help: '' }));
}

export async function providerOptions(event: KitEvent): Promise<Option[]> {
	const providers = await event.locals.api.GET('/providers', {
		params: { query: { limit: LIMIT } }
	});
	return (providers.data?.items ?? []).map((p) => ({
		value: p.id,
		label: p.display_name && p.display_name !== p.name ? `${p.display_name} (${p.name})` : p.name,
		help: ''
	}));
}

/** A model as pickers and editors need it. */
export interface ModelBrief {
	id: string;
	/** The id applications use, such as `anthropic/claude-sonnet`. */
	name: string;
	state: string;
	successor: string | null;
	capabilities: Record<string, unknown>;
}

/** Every model of the repository, page after page. */
export async function everyModel(api: KitEvent['locals']['api']) {
	const items = [];
	let cursor: string | undefined;
	for (let page = 0; page < 40; page++) {
		const models = await api.GET('/models', {
			params: { query: { limit: 500, ...(cursor ? { cursor } : {}) } }
		});
		items.push(...(models.data?.items ?? []));
		cursor = models.data?.next_cursor ?? undefined;
		if (!cursor) break;
	}
	return items;
}

/** Every model of the repository. */
export async function allModels(event: KitEvent): Promise<ModelBrief[]> {
	return (await everyModel(event.locals.api)).map((m) => ({
		id: m.id,
		name: m.name,
		state: m.state,
		successor: m.successor ?? null,
		capabilities: { ...m.capabilities }
	}));
}

/** "anthropic/claude-sonnet (deprecated)": the state is said unless the model is active. */
export function modelLabel(model: ModelBrief): string {
	return model.state === 'active' ? model.name : `${model.name} (${model.state})`;
}

/** Models as choices, valued by object id or by model id. */
export function modelOptions(models: ModelBrief[], by: 'id' | 'name' = 'id'): Option[] {
	return models.map((m) => ({ value: m[by], label: modelLabel(m), help: '' }));
}

/**
 * The models a workspace may use: those granted to its organisation, narrowed to the
 * workspace's own grants when it has any. Without `ws`, the organisation's.
 */
export async function grantedModels(
	event: KitEvent,
	org: string,
	ws?: string
): Promise<ModelBrief[]> {
	const [models, orgGrants, wsGrants] = await Promise.all([
		allModels(event),
		event.locals.api.GET('/orgs/{org}/grants', {
			params: { path: { org }, query: { limit: LIMIT } }
		}),
		ws
			? event.locals.api.GET('/orgs/{org}/workspaces/{ws}/grants', {
					params: { path: { org, ws }, query: { limit: LIMIT } }
				})
			: null
	]);
	const ofOrg = new Set((orgGrants.data?.items ?? []).map((g) => g.model_id));
	const ofWs = new Set((wsGrants?.data?.items ?? []).map((g) => g.model_id));
	return models.filter((m) => ofOrg.has(m.id) && (ofWs.size === 0 || ofWs.has(m.id)));
}

/** The names of a workspace's templates. */
export async function templateOptions(event: KitEvent, org: string, ws: string): Promise<Option[]> {
	const templates = await event.locals.api.GET('/orgs/{org}/workspaces/{ws}/templates', {
		params: { path: { org, ws }, query: { limit: LIMIT } }
	});
	return (templates.data?.items ?? []).map((t) => ({
		value: t.name,
		label: t.description ? `${t.name} – ${t.description}` : t.name,
		help: ''
	}));
}

/** The API keys of a workspace, revoked ones marked, for limits aimed at keys. */
export async function keyOptions(event: KitEvent, org: string, ws: string): Promise<Option[]> {
	const keys = await event.locals.api.GET('/orgs/{org}/workspaces/{ws}/keys', {
		params: { path: { org, ws }, query: { limit: LIMIT } }
	});
	return (keys.data?.items ?? []).map((k) => ({
		value: k.id,
		label: k.status === 'active' ? k.name : `${k.name} (revoked)`,
		help: ''
	}));
}

/** The rate classes keys may name. */
export async function rateClassOptions(event: KitEvent): Promise<Option[]> {
	const classes = await event.locals.api.GET('/rate-classes', {
		params: { query: { limit: LIMIT } }
	});
	return (classes.data?.items ?? []).map((c) => {
		const parts = [
			c.requests_per_minute ? `${c.requests_per_minute} calls/min` : '',
			c.tokens_per_minute ? `${c.tokens_per_minute} tokens/min` : '',
			c.max_concurrency ? `${c.max_concurrency} at once` : ''
		].filter(Boolean);
		return {
			value: c.name,
			label: parts.length ? `${c.name} – ${parts.join(', ')}` : c.name,
			help: ''
		};
	});
}
