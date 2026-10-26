import { describe, expect, it } from 'vitest';
import { exampleAfterSwitch, playgroundExample } from './playground';

describe('playgroundExample', () => {
	it('spells the token limit as each interface does', () => {
		expect(playgroundExample('openai', 'acme/m')).toEqual({
			model: 'acme/m',
			messages: [{ role: 'user', content: 'Say hello in one sentence.' }],
			max_tokens: 200
		});
		expect(playgroundExample('anthropic', 'acme/m')).toMatchObject({ max_tokens: 200 });
		const native = playgroundExample('native', 'acme/m');
		expect(native).toEqual({
			model: 'acme/m',
			messages: [{ role: 'user', content: [{ type: 'text', text: 'Say hello in one sentence.' }] }],
			max_output_tokens: 200
		});
		expect(native).not.toHaveProperty('max_tokens');
	});
});

describe('exampleAfterSwitch', () => {
	const shown = (iface: 'openai' | 'anthropic' | 'native', model = 'acme/m') =>
		JSON.stringify(playgroundExample(iface, model), null, 2);

	it('replaces an example with the new interface’s, keeping its model', () => {
		const next = exampleAfterSwitch('native', shown('openai', '@planner'));
		expect(JSON.parse(next ?? '')).toEqual(playgroundExample('native', '@planner'));
		expect(JSON.parse(exampleAfterSwitch('openai', shown('native')) ?? '')).toEqual(
			playgroundExample('openai', 'acme/m')
		);
	});

	it('fills an empty body', () => {
		expect(JSON.parse(exampleAfterSwitch('anthropic', '  ') ?? '')).toEqual(
			playgroundExample('anthropic', '')
		);
	});

	it('keeps a body edited by hand, and one that is not JSON', () => {
		const edited = JSON.stringify({ ...playgroundExample('openai', 'acme/m'), temperature: 0 });
		expect(exampleAfterSwitch('native', edited)).toBeNull();
		expect(exampleAfterSwitch('native', '{"model": ')).toBeNull();
		expect(exampleAfterSwitch('native', '[1]')).toBeNull();
	});

	it('ignores a value that is not a playground interface', () => {
		expect(exampleAfterSwitch('gemini', shown('openai'))).toBeNull();
	});
});
