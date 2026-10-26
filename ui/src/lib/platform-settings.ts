/**
 * The settings of the installation, by topic: one tab each, saved on its own. Every field of
 * the settings is in exactly one topic; a test holds the list to the OpenAPI document.
 */
import type { Tab } from './nav';

export interface SettingsTopic {
	/** The last step of the page's path. */
	key: string;
	/** The tab. */
	label: string;
	title: string;
	description: string;
	/** The field paths the page shows, in this order. */
	paths: string[];
}

export const SETTINGS_TOPICS: SettingsTopic[] = [
	{
		key: 'calls',
		label: 'Calls',
		title: 'Calls',
		description:
			'What calls may ask for and how they are handled. Organisations and workspaces can override the defaults that are theirs to set.',
		paths: [
			'unsupported_params',
			'provider_tools',
			'anthropic_betas_allowed',
			'on_spool_full',
			'tag_keys'
		]
	},
	{
		key: 'routing',
		label: 'Routing',
		title: 'Routing and models',
		description:
			'When a failing model is set aside, the targets for the gateway delay, and how often providers are asked for their models.',
		paths: ['breaker', 'delay_targets', 'discovery_interval_hours']
	},
	{
		key: 'audit',
		label: 'Audit',
		title: 'Audit and retention',
		description:
			'Recording calls in full, how long usage is kept, and the responses of the Responses API the gateway keeps. Organisations and workspaces can override the defaults that are theirs to set. Realtime sessions, and files and batches, are recorded only with their own options; what the gateway cannot record is then served without a record, unless it is to be refused.',
		paths: ['audit_rule', 'audit', 'audit_max_body_bytes', 'usage_retention_days', 'responses']
	},
	{
		key: 'costs',
		label: 'Costs',
		title: 'Currency and holidays',
		description:
			'The currency costs are counted in, the rates that convert other currencies to it, and the public holidays of off-peak prices.',
		paths: ['reporting_currency', 'exchange_rates', 'holiday_calendars']
	},
	{
		key: 'sign-in',
		label: 'Sign-in',
		title: 'Sign-in',
		description:
			'Console sessions and the links sent by email. With sign-in through pacod-id, its realm settings apply instead.',
		paths: [
			'require_verified_email',
			'session_hours',
			'remember_me_days',
			'verification_link_hours',
			'reset_link_minutes',
			'invitation_link_days'
		]
	}
];

/** The topic of a page, by its key. */
export function settingsTopic(key: string): SettingsTopic | undefined {
	return SETTINGS_TOPICS.find((topic) => topic.key === key);
}

/** The path of a topic's page. */
export function settingsPath(key: string): string {
	return `/platform/settings/${key}`;
}

export const SETTINGS_TABS: Tab[] = SETTINGS_TOPICS.map((topic) => ({
	label: topic.label,
	href: settingsPath(topic.key)
}));
