import { organisationPath } from '#lib/nav.ts';
import type { KitEvent } from '#lib/server/resource.ts';
import { schemeOptions } from '#lib/server/routing.ts';
import { countOf, settingsResource } from '#lib/server/settings.ts';

/** The settings an organisation can keep its workspaces from overriding. */
const LOCKABLE = [
	{ value: 'unsupported_params', label: 'Unsupported parameters', help: '' },
	{ value: 'provider_tools', label: 'Provider tools', help: '' },
	{ value: 'audit', label: 'Audit mode', help: '' },
	{
		value: 'routing_scheme',
		label: 'Routing scheme',
		help: 'Also keeps templates from naming a scheme of their own.'
	}
];

const settings = settingsResource({
	path: '/orgs/{org}',
	noun: 'organisation',
	writers: 'organisation',
	// Switching an organisation off, and removing it, is the platform's decision.
	statusWriters: 'platform',
	deleters: 'platform',
	sections: [
		{
			key: 'general',
			title: 'General',
			description:
				'How the organisation is called. The name is its short identifier in addresses and exports; only platform admins change it.',
			paths: ['display_name', 'name']
		},
		{
			key: 'defaults',
			title: 'Defaults for workspaces',
			description:
				'What its workspaces use unless they set their own. Left unset, the installation’s setting applies.',
			paths: ['settings.unsupported_params', 'settings.provider_tools', 'settings.routing_scheme']
		},
		{
			key: 'audit',
			title: 'Audit mode',
			description:
				'Recording calls in full, prompts and answers included. The rule can rule audit mode out, or make it mandatory, for every workspace.',
			paths: ['settings.audit']
		},
		{
			key: 'locks',
			title: 'Locks',
			description:
				'A locked setting is the organisation’s for all its workspaces: what a workspace sets for it is ignored.',
			paths: ['settings.locked']
		}
	],
	fields: async (event) => ({
		'settings.unsupported_params': { label: 'Unsupported parameters' },
		'settings.audit': { label: 'Audit mode of the organisation', help: '' },
		'settings.locked': {
			label: 'Locked settings',
			help: '',
			options: LOCKABLE
		},
		'settings.routing_scheme': {
			label: 'Routing scheme',
			help: 'For templates with several models that name no scheme, in workspaces that name none.',
			options: await schemeOptions(event, event.params.org ?? '')
		}
	}),
	readOnly: (event) => (event.locals.principal?.platform_admin ? [] : ['name']),
	afterDelete: () => '/platform/organisations'
});

export const load = async (event: KitEvent) => {
	const org = event.params.org ?? '';
	const [data, workspaces, grants, limits, schemes] = await Promise.all([
		settings.load(event),
		countOf(event, '/orgs/{org}/workspaces', { org }),
		countOf(event, '/orgs/{org}/grants', { org }),
		countOf(event, '/orgs/{org}/limits', { org }),
		countOf(event, '/orgs/{org}/routing-schemes', { org })
	]);
	const entry = (label: string, page: string, c: Awaited<ReturnType<typeof countOf>>) => ({
		label,
		count: c?.count ?? null,
		more: c?.more ?? false,
		href: organisationPath(org, page)
	});
	return {
		...data,
		inside: [
			entry('Workspaces', 'workspaces', workspaces),
			entry('Model grants', 'grants', grants),
			entry('Limits', 'limits', limits),
			entry('Routing schemes', 'routing', schemes)
		]
	};
};

export const actions = settings.actions;
