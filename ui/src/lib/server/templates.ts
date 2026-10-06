/** What the template editor needs besides the template itself. */
import { canWrite } from '#lib/access.ts';
import { buildFields, type Field } from '#lib/schema/form.ts';
import { HAND_BUILT } from '#lib/templates.ts';
import { grantedModels, type ModelBrief } from './options.ts';
import type { KitEvent } from './resource.ts';
import { spec } from './spec.ts';

const COLLECTION = '/orgs/{org}/workspaces/{ws}/templates';

/** The fields of a template that are a plain form; the editor renders the others itself. */
export function plainTemplateFields(): Field[] {
	const input = spec.paths[COLLECTION]?.post?.requestBody?.content?.['application/json']?.schema;
	if (input === undefined) return [];
	return buildFields(spec, input, {
		overrides: {
			...Object.fromEntries(HAND_BUILT.map((path) => [path, { hidden: true }])),
			name: {
				help: 'Slug, unique in the workspace. Applications address the template as `@name`.'
			},
			comment: { label: 'What this version changes', help: 'A note for the list of versions.' },
			'spec.max_output_tokens': { label: 'Output-token limit' },
			'spec.timeout_seconds': { label: 'Timeout in seconds' },
			'spec.retry_max_attempts': { label: 'Attempts' },
			'spec.prompt_cache': { label: 'Prompt-cache markers' },
			'spec.unsupported_params': { label: 'Parameters the model does not accept' },
			'spec.audit': { label: 'Audit mode' },
			'spec.proxy': { label: 'In proxy mode' },
			'spec.request_stream_usage': { label: 'Ask for usage on streams' }
		}
	});
}

/** A routing scheme a template can name. */
export interface SchemeOption {
	name: string;
	description: string;
	active: boolean;
}

/** The scheme a template gets when it names none. */
export interface InheritedRouting {
	scheme: string | null;
	source: 'workspace' | 'organisation' | null;
	/** The organisation decides for all its templates. */
	locked: boolean;
}

export interface EditorContext {
	/** The models the workspace may use. */
	models: ModelBrief[];
	/** The organisation's routing schemes. */
	schemes: SchemeOption[];
	inherited: InheritedRouting;
	/** The tag keys the workspace declares. */
	tagKeys: string[];
	fields: Field[];
	/** Whether the caller may save. */
	writable: boolean;
}

export async function editorContext(event: KitEvent): Promise<EditorContext> {
	const org = event.params.org ?? '';
	const ws = event.params.ws ?? '';
	const [models, workspace, organisation, schemes] = await Promise.all([
		grantedModels(event, org, ws),
		event.locals.api.GET('/orgs/{org}/workspaces/{ws}', { params: { path: { org, ws } } }),
		event.locals.api.GET('/orgs/{org}', { params: { path: { org } } }),
		event.locals.api.GET('/orgs/{org}/routing-schemes', {
			params: { path: { org }, query: { limit: 500 } }
		})
	]);
	const principal = event.locals.principal;
	const orgSettings = organisation.data?.settings;
	const locked = orgSettings?.locked?.includes('routing_scheme') ?? false;
	const wsScheme = locked ? null : (workspace.data?.settings?.routing_scheme ?? null);
	const orgScheme = orgSettings?.routing_scheme ?? null;
	return {
		models,
		schemes: (schemes.data?.items ?? []).map((s) => ({
			name: s.name,
			description: s.description,
			active: s.status === 'active'
		})),
		inherited: {
			scheme: wsScheme ?? orgScheme,
			source: wsScheme ? 'workspace' : orgScheme ? 'organisation' : null,
			locked
		},
		tagKeys: workspace.data?.tag_keys ?? [],
		fields: plainTemplateFields(),
		writable: principal !== null && canWrite(principal, 'workspace', { org, ws })
	};
}

/** The tag keys alone, for reading a submitted form. */
export async function declaredTagKeys(event: KitEvent): Promise<string[]> {
	const workspace = await event.locals.api.GET('/orgs/{org}/workspaces/{ws}', {
		params: { path: { org: event.params.org ?? '', ws: event.params.ws ?? '' } }
	});
	return workspace.data?.tag_keys ?? [];
}
