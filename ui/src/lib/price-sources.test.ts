import { describe, expect, it } from 'vitest';
import { LITELLM_PRICES, defaultPriceSources } from './price-sources';

const feed = { kind: 'feed', url: LITELLM_PRICES, format: 'litellm' };

describe('defaultPriceSources', () => {
	it('offers LiteLLM for the five providers it covers, told by their address', () => {
		const providers = [
			{ id: 'prv_g', name: 'glm', base_url: 'https://api.z.ai/api/paas/v4' },
			{ id: 'prv_o', name: 'openai', base_url: 'https://api.openai.com/v1' },
			{ id: 'prv_a', name: 'claude', base_url: 'https://api.anthropic.com' },
			{ id: 'prv_d', name: 'deepseek', base_url: 'https://api.deepseek.com' },
			{ id: 'prv_n', name: 'gemini', base_url: 'https://generativelanguage.googleapis.com/v1beta' },
			{
				id: 'prv_c',
				name: 'gemini-openai',
				base_url: 'https://generativelanguage.googleapis.com/v1beta/openai'
			}
		];
		expect(defaultPriceSources(providers)).toEqual([
			{ provider_id: 'prv_a', ...feed },
			{ provider_id: 'prv_d', ...feed, key_prefix: 'deepseek' },
			{ provider_id: 'prv_n', ...feed, key_prefix: 'gemini' },
			{ provider_id: 'prv_c', ...feed, key_prefix: 'gemini' },
			{ provider_id: 'prv_g', ...feed, key_prefix: 'zai' },
			{ provider_id: 'prv_o', ...feed }
		]);
	});

	it('leaves out other addresses, whatever the provider is called', () => {
		const providers = [
			{ id: 'prv_1', name: 'openai', base_url: 'http://gw-mock:9280/v1' },
			{ id: 'prv_2', name: 'glm-cn', base_url: 'https://open.bigmodel.cn/api/paas/v4' },
			{
				id: 'prv_3',
				name: 'vertex',
				base_url: 'https://aiplatform.googleapis.com/v1/publishers/google'
			},
			{ id: 'prv_4', name: 'kimi', base_url: 'https://api.moonshot.ai/v1' },
			{ id: 'prv_5', name: 'broken', base_url: 'not a url' }
		];
		expect(defaultPriceSources(providers)).toEqual([]);
	});
});
