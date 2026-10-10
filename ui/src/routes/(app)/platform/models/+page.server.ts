import type { Field, FieldOverrides } from '#lib/schema/form.ts';
import { readForm } from '#lib/schema/form.ts';
import { stateWord } from '#lib/models.ts';
import { allModels, modelOptions, providerOptions } from '#lib/server/options.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	resource,
	type KitEvent
} from '#lib/server/resource.ts';

const models = resource({
	path: '/models',
	noun: 'model',
	fields: async (event) => {
		const [providers, others] = await Promise.all([providerOptions(event), allModels(event)]);
		return {
			provider_id: { label: 'Provider', options: providers },
			successor_id: { label: 'Successor', options: modelOptions(others) },
			// The preset picker of the page fills the capability fields; what is sent is the
			// profile itself, which wins over a preset anyway.
			capability_preset: { hidden: true },
			capabilities: { required: true }
		};
	},
	extra: async (event) => {
		const presets = await event.locals.api.GET('/capability-presets');
		return { presets: presets.data ?? [] };
	}
});

/** The inputs of a transition that matter for the state it leads to. */
function transitionFields(to: string, from: string, successors: FieldOverrides[string]): Field[] {
	const hidden = { hidden: true };
	return inputFields('/models/{id}/transition', 'post', {
		to: hidden,
		successor_id: to === 'deprecated' || to === 'retired' ? successors : hidden,
		sunset_at: to === 'deprecated' ? {} : hidden,
		retire_at_sunset:
			to === 'deprecated'
				? {
						label: 'Retire it at the sunset',
						help: 'The gateway retires the model by itself once the sunset has passed.'
					}
				: hidden,
		skip_probe: to === 'active' && from === 'draft' ? {} : hidden
	});
}

async function successorChoices(event: KitEvent, self: string): Promise<FieldOverrides[string]> {
	const others = (await allModels(event)).filter((m) => m.id !== self);
	return {
		label: 'Successor',
		help: 'The model that replaces this one. Requests follow it once this model is retired, where the grant allows.',
		options: modelOptions(others)
	};
}

export const load = models.load;

export const actions = {
	...models.actions,

	/** A dry run of a state change: what it would affect. Nothing is changed. */
	preview: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const to = formText(form, 'to');
		const result = await event.locals.api.POST('/models/{id}/transition', {
			params: { path: { id }, query: { dry_run: 'true' } },
			body: {
				to: to as never,
				successor_id: null,
				sunset_at: null,
				retire_at_sunset: null,
				skip_probe: false
			}
		});
		if (!result.response.ok || !result.data) return failed(event, 'preview', result, { id });
		const { from, impact, model } = result.data;
		return done('preview', '', {
			id,
			data: {
				to,
				from,
				impact,
				model: {
					id: model.id,
					name: model.name,
					successor_id: model.successor_id ?? null,
					sunset_at: model.sunset_at ?? null
				},
				fields: transitionFields(to, from, await successorChoices(event, id))
			}
		});
	},

	/** Applies a state change. */
	transition: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const to = formText(form, 'to');
		const fields = transitionFields(to, formText(form, 'from'), await successorChoices(event, id));
		const input = readForm(fields, form);
		const result = await event.locals.api.POST('/models/{id}/transition', {
			params: { path: { id } },
			body: {
				to: to as never,
				successor_id: typeof input.successor_id === 'string' ? input.successor_id : null,
				sunset_at: typeof input.sunset_at === 'string' ? input.sunset_at : null,
				retire_at_sunset: to === 'deprecated' ? input.retire_at_sunset === true : null,
				skip_probe: input.skip_probe === true
			}
		});
		if (!result.response.ok || !result.data)
			return failed(event, 'transition', result, { id, fields });
		return done(
			'transition',
			`Model “${result.data.model.name}” is now ${stateWord(result.data.to)}.`,
			{ id }
		);
	},

	/** Calls the provider to check that it knows the model. */
	probe: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const name = formText(form, 'name');
		const result = await event.locals.api.POST('/models/{id}/probe', { params: { path: { id } } });
		if (!result.response.ok || !result.data) return failed(event, 'probe', result, { id });
		const { ok, detail, latency_ms } = result.data;
		return done(
			'probe',
			ok
				? `The provider answered for “${name}” in ${latency_ms} ms${detail ? `: ${detail}` : '.'}`
				: `The probe of “${name}” failed after ${latency_ms} ms: ${detail}`,
			{ id, tone: ok ? 'success' : 'warning' }
		);
	}
};
