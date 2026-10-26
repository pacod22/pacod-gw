/**
 * The example request of the workspace playground, in the shape of each interface it offers.
 * The shapes come from the Testing page's composer, so that the two pages agree.
 */

import { buildBody, defaultDraft } from './message.ts';

export type PlaygroundInterface = 'openai' | 'anthropic' | 'native';

const PROMPT = 'Say hello in one sentence.';
const MAX_TOKENS = '200';

const INTERFACES: PlaygroundInterface[] = ['openai', 'anthropic', 'native'];

/** The example body for an interface: each spells the token limit its own way. */
export function playgroundExample(
	iface: PlaygroundInterface,
	model: string
): Record<string, unknown> {
	return (
		buildBody({
			...defaultDraft(),
			interface: iface,
			model,
			messages: [{ role: 'user', content: PROMPT }],
			maxTokens: MAX_TOKENS
		}) ?? { model }
	);
}

/**
 * The body text to show after the interface changed, or null to keep the current one. Only an
 * empty body or an example is replaced, with its model carried over: a body edited by hand is
 * the user's work and stays as it is.
 */
export function exampleAfterSwitch(iface: string, current: string): string | null {
	if (!INTERFACES.includes(iface as PlaygroundInterface)) return null;
	let model = '';
	if (current.trim()) {
		let parsed: unknown;
		try {
			parsed = JSON.parse(current);
		} catch {
			return null;
		}
		if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
		model = String((parsed as Record<string, unknown>).model ?? '');
		const text = JSON.stringify(parsed);
		if (!INTERFACES.some((i) => JSON.stringify(playgroundExample(i, model)) === text)) return null;
	}
	return JSON.stringify(playgroundExample(iface as PlaygroundInterface, model), null, 2);
}
