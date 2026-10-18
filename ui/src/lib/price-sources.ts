/**
 * The sources the price tracker's form offers until the job is first saved (concept section
 * 13.3): LiteLLM's public price list, for each provider at one of the addresses it covers.
 */
import type { Schemas } from './api/types';

/** LiteLLM's public price list: rates per token, in US dollars. */
export const LITELLM_PRICES =
	'https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json';

/**
 * The providers' hosts, with the prefix LiteLLM files their models under where it is not the
 * model's id alone. GLM's mainland endpoint (open.bigmodel.cn) is not here: it bills in CNY,
 * while LiteLLM's `zai` entries are z.ai's prices in US dollars. Vertex AI is not here either:
 * LiteLLM lists its prices apart, under `vertex_ai/`.
 */
const HOSTS: Record<string, string | undefined> = {
	'api.openai.com': undefined,
	'api.anthropic.com': undefined,
	'generativelanguage.googleapis.com': 'gemini',
	'api.deepseek.com': 'deepseek',
	'api.z.ai': 'zai'
};

/**
 * A LiteLLM source for each provider whose base URL is OpenAI's, Anthropic's, Gemini's,
 * DeepSeek's or z.ai's, by provider name. The provider is told by its address, not its name:
 * names are free, and DeepSeek and GLM share the OpenAI family.
 */
export function defaultPriceSources(
	providers: { id: string; name: string; base_url: string }[]
): Schemas['PriceSourceSpec'][] {
	return providers
		.filter((p) => Object.hasOwn(HOSTS, hostOf(p.base_url)))
		.sort((a, b) => a.name.localeCompare(b.name, 'en', { numeric: true }))
		.map((p) => {
			const prefix = HOSTS[hostOf(p.base_url)];
			return {
				provider_id: p.id,
				kind: 'feed',
				url: LITELLM_PRICES,
				format: 'litellm',
				...(prefix ? { key_prefix: prefix } : {})
			};
		});
}

function hostOf(url: string): string {
	try {
		return new URL(url).hostname.toLowerCase();
	} catch {
		return '';
	}
}
