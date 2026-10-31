/**
 * The life cycle of a model, as far as the console needs it to offer
 * the right buttons. The core decides; a transition it refuses is shown as it words it.
 */

export type ModelState = 'draft' | 'active' | 'deprecated' | 'retired' | 'disabled';

export interface StateChange {
	to: ModelState;
	/** What the button says. */
	label: string;
	/** What the change means, for the person choosing. */
	help: string;
}

const HELP: Record<ModelState, string> = {
	draft: 'Being set up. Not callable.',
	active: 'Callable by every workspace it is granted to.',
	deprecated:
		'Still callable. Responses carry a deprecation notice and new templates may not choose it.',
	retired: 'Not callable itself. Requests follow the successor where the grant allows it.',
	disabled: 'Emergency off switch. The model returns to the state it had before.'
};

export function stateHelp(state: string): string {
	return HELP[state as ModelState] ?? '';
}

/** The changes the core accepts from a state. Mirrors `ModelState::can_become`. */
export function stateChanges(state: string, previous?: string | null): StateChange[] {
	const change = (to: ModelState, label: string): StateChange => ({ to, label, help: HELP[to] });
	switch (state) {
		case 'draft':
			return [change('active', 'Activate')];
		case 'active':
			return [
				change('deprecated', 'Deprecate'),
				change('retired', 'Retire'),
				change('disabled', 'Disable')
			];
		case 'deprecated':
			return [
				change('retired', 'Retire'),
				change('active', 'Take the deprecation back'),
				change('disabled', 'Disable')
			];
		case 'retired':
			return [change('active', 'Reactivate'), change('deprecated', 'Return to deprecated')];
		case 'disabled': {
			const back = (previous ?? 'active') as ModelState;
			return [{ to: back, label: `Enable again as ${back}`, help: HELP[back] }];
		}
		default:
			return [];
	}
}

/** The past participle for messages: "is now deprecated". */
export function stateWord(state: string): string {
	return state === 'active' ? 'active' : state;
}

/** The organisation of the gateway's own calls (`SYSTEM_ORG_ID` in the core). */
export const SYSTEM_ORGANISATION = 'org_system';

/**
 * The models a workspace may use, decided as the core decides it (`Snapshot::grant_terms`):
 * those granted to its organisation, narrowed to the workspace's own grants when it has any.
 * The system organisation holds every active and deprecated model without a stored grant,
 * so its grants alone would leave out every model added since.
 */
export function usableModels<M extends { id: string; state: string }>(
	models: M[],
	org: string,
	orgGrants: string[],
	wsGrants: string[] = []
): M[] {
	const ofOrg = new Set(orgGrants);
	if (org === SYSTEM_ORGANISATION) {
		for (const m of models) if (m.state === 'active' || m.state === 'deprecated') ofOrg.add(m.id);
	}
	const ofWs = new Set(wsGrants);
	return models.filter((m) => ofOrg.has(m.id) && (ofWs.size === 0 || ofWs.has(m.id)));
}
