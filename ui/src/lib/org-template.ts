/**
 * The organisation template in the console: what loading it would change in an organisation,
 * and what it changed. Loading never overwrites: a setting the organisation already holds is
 * kept, and a routing scheme whose name is taken is not created again. Loading it twice
 * therefore changes nothing the second time.
 */
import type { Schemas } from '#lib/api/types.ts';
import { formatList } from '#lib/format.ts';
import { planSchemes, schemesPhrase, type SchemeStep } from '#lib/routing.ts';

export type OrgConfigTemplate = Schemas['OrgConfigTemplate'];
type ScopeSettings = Schemas['ScopeSettings'];
type AuditSettings = Schemas['AuditSettings'];

const LABELS: Record<string, string> = {
	unsupported_params: 'Unsupported parameters',
	provider_tools: 'Provider tools',
	audit: 'Audit mode',
	routing_scheme: 'Routing scheme',
	on_spool_full: 'When the event spool is full',
	usage_retention_days: 'Usage retention',
	tag_keys: 'Tag keys',
	locked: 'Locked settings'
};

/** One setting the template holds, against what the organisation has now. */
export interface SettingStep {
	key: string;
	label: string;
	/** What the organisation holds now; `null` while it inherits. */
	now: string | null;
	value: string;
	reason: string;
	/** Whether loading sets it: only where the organisation has no value of its own. */
	apply: boolean;
}

export interface TemplatePlan {
	settings: SettingStep[];
	schemes: SchemeStep[];
	/** How many settings and schemes loading would add. */
	changes: number;
}

function isSet(value: unknown): boolean {
	return value !== null && value !== undefined && !(Array.isArray(value) && value.length === 0);
}

function describeAudit(a: AuditSettings): string {
	const modes: Record<string, string> = {
		off: 'Off',
		on: 'Every call',
		on_request: 'On request'
	};
	const parts = [modes[a.mode ?? 'off'] ?? String(a.mode)];
	if (a.sample) parts.push(`${Math.round(a.sample * 100)}% of calls`);
	if (a.attachments === 'reference') parts.push('attachments as size and hash');
	if (a.retention_days) parts.push(`kept ${a.retention_days} days`);
	if (a.guarantee === 'required') parts.push('calls refused while unrecorded');
	if (a.realtime) parts.push('Realtime sessions too');
	if (a.rule)
		parts.push(a.rule === 'force' ? 'mandatory for workspaces' : 'ruled out for workspaces');
	return parts.join(', ');
}

/** A setting's value in a few words: "Drop and report", "On request, kept 14 days". */
export function describeSetting(key: string, value: unknown): string {
	if (key === 'unsupported_params') {
		return value === 'reject'
			? 'Reject the call'
			: value === 'drop'
				? 'Drop and report'
				: String(value);
	}
	if (key === 'audit' && typeof value === 'object' && value !== null) {
		return describeAudit(value as AuditSettings);
	}
	if (Array.isArray(value)) {
		if (value.length === 0) return 'None';
		return value.map((v) => (key === 'locked' ? (LABELS[v] ?? v) : String(v))).join(', ');
	}
	return typeof value === 'string' ? value : JSON.stringify(value);
}

/** What loading the template into an organisation with `current` and the schemes `taken` would do. */
export function planTemplate(
	template: OrgConfigTemplate,
	current: ScopeSettings | null | undefined,
	taken: string[]
): TemplatePlan {
	const mine = (current ?? {}) as Record<string, unknown>;
	const settings = Object.entries(template.settings as Record<string, unknown>)
		.filter(([, value]) => isSet(value))
		.map(([key, value]) => ({
			key,
			label: LABELS[key] ?? key,
			now: isSet(mine[key]) ? describeSetting(key, mine[key]) : null,
			value: describeSetting(key, value),
			reason: template.reasons[key] ?? '',
			apply: !isSet(mine[key])
		}));
	const schemes = planSchemes(template.routing_schemes, taken);
	const changes = settings.filter((s) => s.apply).length + schemes.filter((s) => s.create).length;
	return { settings, schemes, changes };
}

/** The organisation's settings with what the template sets put where it has none. */
export function fillSettings(
	template: OrgConfigTemplate,
	current: ScopeSettings | null | undefined
): ScopeSettings {
	const filled: Record<string, unknown> = { ...(current ?? {}) };
	for (const [key, value] of Object.entries(template.settings as Record<string, unknown>)) {
		if (isSet(value) && !isSet(filled[key])) filled[key] = value;
	}
	return filled as ScopeSettings;
}

/** What loading did, in a sentence. */
export function loadedMessage(created: string[], set: string[]): string {
	if (created.length === 0 && set.length === 0) {
		return 'Nothing to load: the organisation already has everything the template holds.';
	}
	const parts: string[] = [];
	if (created.length > 0) parts.push(`created ${schemesPhrase(created)}`);
	if (set.length > 0) parts.push(`set ${formatList(set.map((s) => s.toLowerCase()))}`);
	return `Template loaded: ${parts.join('; ')}.`;
}
