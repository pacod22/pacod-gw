import { bindingPrepare } from '#lib/plugins.ts';
import {
	bindingFields,
	bindingTemplate,
	clearCache,
	compiledPlugins,
	intentDemo,
	scopeCache
} from '#lib/server/plugins.ts';
import { done, failed, formText, resource, type KitEvent } from '#lib/server/resource.ts';

const bindings = resource({
	path: '/orgs/{org}/workspaces/{ws}/plugin-bindings',
	noun: 'plugin binding',
	fields: (event) => bindingFields(event, event.params.org ?? '', event.params.ws ?? ''),
	prepare: bindingPrepare,
	extra: async (event) => {
		const [template, demo, plugins, cache] = await Promise.all([
			bindingTemplate(event, 'workspace'),
			intentDemo(event),
			compiledPlugins(event),
			scopeCache(event, 'workspace')
		]);
		return { ...template, ...demo, plugins: plugins.plugins, ...cache };
	}
});

type Engine = 'llm' | 'jev' | 'cascade';
const ENGINES: Engine[] = ['llm', 'jev', 'cascade'];

function enginesOf(form: FormData): Engine[] {
	return form.getAll('engines').filter((e): e is Engine => ENGINES.includes(e as Engine));
}

const path = (event: KitEvent) => ({ org: event.params.org ?? '', ws: event.params.ws ?? '' });

export const load = bindings.load;

export const actions = {
	...bindings.actions,

	clearCache: (event: KitEvent) => clearCache(event, 'workspace'),

	/** Classifies one text with each engine chosen. The engines' calls are real. */
	try: async (event: KitEvent) => {
		const form = await event.request.formData();
		const result = await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/plugins/intent/try', {
			params: { path: path(event) },
			body: { text: formText(form, 'text'), engines: enginesOf(form) }
		});
		if (!result.response.ok || !result.data) return failed(event, 'try', result);
		return done('try', '', { data: { ...result.data, text: formText(form, 'text') } });
	},

	/**
	 * Classifies a labelled set: one JSON object per line, `{"text": …, "intent": …}`, or a
	 * line `intent<TAB>text`.
	 */
	evaluate: async (event: KitEvent) => {
		const form = await event.request.formData();
		const samples: { text: string; intent: string }[] = [];
		for (const line of formText(form, 'samples').split('\n')) {
			const text = line.trim();
			if (!text) continue;
			if (text.startsWith('{')) {
				try {
					const v = JSON.parse(text) as { text?: unknown; intent?: unknown };
					samples.push({ text: String(v.text ?? ''), intent: String(v.intent ?? '') });
				} catch {
					samples.push({ text, intent: '' });
				}
			} else {
				const tab = text.indexOf('\t');
				samples.push(
					tab > 0
						? { intent: text.slice(0, tab).trim(), text: text.slice(tab + 1).trim() }
						: { intent: '', text }
				);
			}
		}
		const result = await event.locals.api.POST(
			'/orgs/{org}/workspaces/{ws}/plugins/intent/evaluate',
			{ params: { path: path(event) }, body: { samples, engines: enginesOf(form) } }
		);
		if (!result.response.ok || !result.data) return failed(event, 'evaluate', result);
		return done('evaluate', '', { data: result.data });
	}
};
