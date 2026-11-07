/** What the template editor needs besides the template itself, at either scope. */
import { fail, redirect } from '@sveltejs/kit';
import { canWrite } from '#lib/access.ts';
import { organisationPath, workspacePath } from '#lib/nav.ts';
import { buildFields, type Field } from '#lib/schema/form.ts';
import { HAND_BUILT, schemeTiers, type TemplateScope } from '#lib/templates.ts';
import { grantedModels, type ModelBrief } from './options.ts';
import { failed, formText, type KitEvent } from './resource.ts';
import { spec } from './spec.ts';

const COLLECTION = {
	workspace: '/orgs/{org}/workspaces/{ws}/templates',
	organisation: '/orgs/{org}/templates'
} as const;

/**
 * Where the templates of a page live: a page below a workspace serves the workspace's, a
 * page below an organisation alone serves the organisation's.
 */
export function templateScope(event: KitEvent): TemplateScope {
	return event.params.ws ? 'workspace' : 'organisation';
}

/** The list of templates the page belongs to, for links and redirects. */
export function templateList(event: KitEvent): string {
	const org = event.params.org ?? '';
	return event.params.ws
		? workspacePath(org, event.params.ws, 'templates')
		: organisationPath(org, 'templates');
}

/**
 * The fields of a template that are a plain form; the editor renders the others itself. Both
 * scopes take the same fields; the help text names where the template lives.
 */
export function plainTemplateFields(scope: TemplateScope = 'workspace'): Field[] {
	const input =
		spec.paths[COLLECTION[scope]]?.post?.requestBody?.content?.['application/json']?.schema;
	if (input === undefined) return [];
	return buildFields(spec, input, {
		overrides: {
			...Object.fromEntries(HAND_BUILT.map((path) => [path, { hidden: true }])),
			name: {
				help:
					scope === 'organisation'
						? 'Slug, unique in the organisation. Applications in every workspace address the template as `@name`.'
						: 'Slug, unique in the workspace. Applications address the template as `@name`.'
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
	/** The tiers its rules and its default choose. */
	tiers: string[];
}

/** The scheme a template gets when it names none. */
export interface InheritedRouting {
	scheme: string | null;
	source: 'workspace' | 'organisation' | null;
	/** The organisation decides for all its templates. */
	locked: boolean;
}

export interface EditorContext {
	scope: TemplateScope;
	/** The models the workspace, or the organisation, may use. */
	models: ModelBrief[];
	/** The organisation's routing schemes. */
	schemes: SchemeOption[];
	inherited: InheritedRouting;
	/** The tag keys a template's tags must be among. */
	tagKeys: string[];
	fields: Field[];
	/** Whether the caller may save. */
	writable: boolean;
}

/**
 * The tag keys of an organisation's templates: those a workspace that declares none gets,
 * which are the organisation's when it has some and the platform's otherwise. The core
 * checks a template's tags against the same list.
 */
async function organisationTagKeys(
	event: KitEvent,
	settings: { tag_keys?: string[] | null } | undefined
): Promise<string[]> {
	const own = settings?.tag_keys ?? [];
	if (own.length > 0) return own;
	const platform = await event.locals.api.GET('/settings');
	return platform.data?.tag_keys ?? [];
}

export async function editorContext(event: KitEvent): Promise<EditorContext> {
	const org = event.params.org ?? '';
	const ws = event.params.ws ?? '';
	const scope = templateScope(event);
	const [models, workspace, organisation, schemes] = await Promise.all([
		scope === 'workspace' ? grantedModels(event, org, ws) : grantedModels(event, org),
		scope === 'workspace'
			? event.locals.api.GET('/orgs/{org}/workspaces/{ws}', { params: { path: { org, ws } } })
			: null,
		event.locals.api.GET('/orgs/{org}', { params: { path: { org } } }),
		event.locals.api.GET('/orgs/{org}/routing-schemes', {
			params: { path: { org }, query: { limit: 500 } }
		})
	]);
	const principal = event.locals.principal;
	const orgSettings = organisation.data?.settings;
	const locked = orgSettings?.locked?.includes('routing_scheme') ?? false;
	const wsScheme = locked ? null : (workspace?.data?.settings?.routing_scheme ?? null);
	const orgScheme = orgSettings?.routing_scheme ?? null;
	return {
		scope,
		models,
		schemes: (schemes.data?.items ?? []).map((s) => ({
			name: s.name,
			description: s.description,
			active: s.status === 'active',
			tiers: schemeTiers([s])
		})),
		inherited: {
			scheme: wsScheme ?? orgScheme,
			source: wsScheme ? 'workspace' : orgScheme ? 'organisation' : null,
			locked
		},
		tagKeys: workspace
			? (workspace.data?.tag_keys ?? [])
			: await organisationTagKeys(event, orgSettings),
		fields: plainTemplateFields(scope),
		writable:
			principal !== null &&
			(scope === 'workspace'
				? canWrite(principal, 'workspace', { org, ws })
				: canWrite(principal, 'organisation', { org }))
	};
}

/** The tag keys alone, for reading a submitted form. */
export async function declaredTagKeys(event: KitEvent): Promise<string[]> {
	const org = event.params.org ?? '';
	if (templateScope(event) === 'organisation') {
		const organisation = await event.locals.api.GET('/orgs/{org}', { params: { path: { org } } });
		return organisationTagKeys(event, organisation.data?.settings);
	}
	const workspace = await event.locals.api.GET('/orgs/{org}/workspaces/{ws}', {
		params: { path: { org, ws: event.params.ws ?? '' } }
	});
	return workspace.data?.tag_keys ?? [];
}

/**
 * Creates a template from a file exported elsewhere (`…/templates/{id}/export`), in the
 * workspace or the organisation of the page.
 */
export async function importTemplate(event: KitEvent) {
	const form = await event.request.formData();
	const org = event.params.org ?? '';
	const ws = event.params.ws ?? '';
	let file: { name?: unknown; description?: unknown; spec?: unknown };
	try {
		const upload = form.get('file');
		const text =
			upload instanceof File && upload.size > 0 ? await upload.text() : formText(form, 'text');
		file = JSON.parse(text);
	} catch {
		return fail(400, {
			action: 'import',
			ok: false,
			message: 'The file is not a template file: it is not JSON.'
		});
	}
	const body = {
		name: formText(form, 'name') || (typeof file.name === 'string' ? file.name : ''),
		description: typeof file.description === 'string' ? file.description : '',
		spec: file.spec as never,
		comment: 'imported'
	};
	const result =
		templateScope(event) === 'workspace'
			? await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/templates', {
					params: { path: { org, ws } },
					body: body as never
				})
			: await event.locals.api.POST('/orgs/{org}/templates', {
					params: { path: { org } },
					body: body as never
				});
	if (!result.response.ok || !result.data) return failed(event, 'import', result);
	redirect(303, `${templateList(event)}/${result.data.id}?created=1`);
}
