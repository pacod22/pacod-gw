import { workspacePath, organisationPath } from '#lib/nav.ts';
import { credentialActions, loadOwnCredentials } from '#lib/server/credentials.ts';
import type { KitEvent } from '#lib/server/resource.ts';
import { timeZoneOptions } from '#lib/server/options.ts';
import { schemeOptions } from '#lib/server/routing.ts';
import { countOf, settingsResource } from '#lib/server/settings.ts';

/** The setting names an organisation locks, and the fields they stand for. */
const LOCKED_FIELDS: Record<string, string> = {
	unsupported_params: 'settings.unsupported_params',
	provider_tools: 'settings.provider_tools',
	audit: 'settings.audit',
	routing_scheme: 'settings.routing_scheme',
	on_spool_full: 'settings.on_spool_full',
	usage_retention_days: 'settings.usage_retention_days',
	tag_keys: 'tag_keys',
	plugin_third_parties: 'settings.plugin_third_parties'
};

const settings = settingsResource({
	path: '/orgs/{org}/workspaces/{ws}',
	noun: 'workspace',
	// A workspace's admins keep its settings; creating and deleting it is the organisation's.
	writers: 'workspace',
	deleters: 'organisation',
	sections: [
		{
			key: 'general',
			title: 'General',
			description:
				'How the workspace is called, the time zone of its calendar limits, and the tag keys its requests may carry.',
			paths: ['display_name', 'name', 'time_zone', 'tag_keys']
		},
		{
			key: 'defaults',
			title: 'Request handling',
			description:
				'Left unset, the organisation’s setting applies, then the installation’s. A setting the organisation locked cannot be changed here.',
			paths: [
				'settings.unsupported_params',
				'settings.provider_tools',
				'settings.routing_scheme',
				'settings.on_spool_full',
				'settings.usage_retention_days',
				'settings.plugin_third_parties'
			]
		},
		{
			key: 'audit',
			title: 'Audit mode',
			description:
				'Recording calls in full, prompts and answers included, for debugging or review. Realtime sessions are recorded only with their own option; sessions the gateway cannot record are then served without a record, unless they are to be refused.',
			paths: ['settings.audit']
		}
	],
	fields: async (event) => ({
		time_zone: { options: timeZoneOptions() },
		'settings.unsupported_params': { label: 'Unsupported parameters' },
		'settings.on_spool_full': { label: 'When the event spool is full' },
		'settings.usage_retention_days': { label: 'Usage retention (days)' },
		'settings.plugin_third_parties': {
			label: 'Plugin third parties',
			help: 'The third parties plugin engines may send text to here, within what the platform approved. Unset: the organisation’s.'
		},
		// The organisation's default tag keys are its own; a workspace declares its own above.
		'settings.tag_keys': { hidden: true },
		'settings.audit': { label: 'Audit mode of the workspace', help: '' },
		'settings.audit.realtime': { label: 'Realtime sessions' },
		'settings.audit.unrecordable': { label: 'Sessions that cannot be recorded' },
		// Ruling audit mode out or making it mandatory is the organisation's decision.
		'settings.audit.rule': { hidden: true },
		'settings.routing_scheme': {
			label: 'Routing scheme',
			help: 'For templates with several models that name no scheme of their own. Unset: the organisation’s.',
			options: await schemeOptions(event, event.params.org ?? '')
		}
	}),
	// What the organisation locked is shown, not offered.
	readOnly: async (event) => {
		const org = await event.locals.api.GET('/orgs/{org}', {
			params: { path: { org: event.params.org ?? '' } }
		});
		return (org.data?.settings?.locked ?? []).map((name) => LOCKED_FIELDS[name]).filter(Boolean);
	},
	afterDelete: (event) => organisationPath(event.params.org ?? '', 'workspaces')
});

export const load = async (event: KitEvent) => {
	const path = { org: event.params.org ?? '', ws: event.params.ws ?? '' };
	const base = '/orgs/{org}/workspaces/{ws}';
	const [data, keys, templates, grants, limits, overrides, credentials] = await Promise.all([
		settings.load(event),
		countOf(event, `${base}/keys`, path),
		countOf(event, `${base}/templates`, path),
		countOf(event, `${base}/grants`, path),
		countOf(event, `${base}/limits`, path),
		countOf(event, `${base}/end-user-overrides`, path),
		loadOwnCredentials(event, { type: 'workspace', id: path.ws })
	]);
	const entry = (label: string, page: string, c: Awaited<ReturnType<typeof countOf>>) => ({
		label,
		count: c?.count ?? null,
		more: c?.more ?? false,
		href: workspacePath(path.org, path.ws, page)
	});
	return {
		...data,
		...credentials,
		inside: [
			entry('API keys, revoked ones included', 'keys', keys),
			entry('Templates', 'templates', templates),
			entry('Model grants', 'grants', grants),
			entry('Limits', 'limits', limits),
			entry('End-user overrides', 'limits/overrides', overrides)
		]
	};
};

export const actions = {
	...settings.actions,
	...credentialActions((event) => ({ type: 'workspace', id: event.params.ws ?? '' }))
};
