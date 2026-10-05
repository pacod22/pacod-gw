<script lang="ts">
	import type { ApiProblem } from '#lib/api/errors.ts';
	import type { Schemas } from '#lib/api/types.ts';
	import { fieldAt, type Field } from '#lib/schema/form.ts';
	import type { ModelBrief } from '#lib/server/options.ts';
	import { capabilityNote, paramOf, REASONING, SAMPLING } from '#lib/templates.ts';
	import ParamRow from './ParamRow.svelte';
	import SchemaField from './SchemaField.svelte';

	/**
	 * The controls of a template. The model, the parameters and the tags are hand-built,
	 * because a parameter has three states and the choices depend on the workspace; the
	 * remaining fields come from the schema like any other form. The surrounding `<form>`
	 * and its buttons belong to the page.
	 */
	interface Props {
		/** The template as it is; none for a new one. */
		template?: Pick<Schemas['Item11'], 'name' | 'description' | 'status' | 'spec'> | null;
		/** The models the workspace may use. */
		models: ModelBrief[];
		/** The tag keys the workspace declares. */
		tagKeys: string[];
		/** The plain fields, from the schema. */
		fields: Field[];
		problem?: ApiProblem | null;
		disabled?: boolean;
	}

	let {
		template = null,
		models,
		tagKeys,
		fields,
		problem = null,
		disabled = false
	}: Props = $props();

	const spec = $derived(template?.spec);
	const errors = $derived(problem?.fields ?? {});
	// The plain form's values: the template without the comment, which belongs to a version.
	const object = $derived(template ? { ...template, comment: '' } : undefined);

	let pickedModel = $state<string | null>(null);
	const modelName = $derived(pickedModel ?? spec?.model ?? '');
	const model = $derived(models.find((m) => m.name === modelName || m.id === modelName));
	// A template may hold a model that is no longer granted: it stays selectable, and is said.
	const unknownModel = $derived(modelName !== '' && !model);
	const efforts = $derived.by(() => {
		const list = model?.capabilities.reasoning_efforts;
		return [...(Array.isArray(list) ? list.map(String) : []), 'none'];
	});
</script>

{#snippet plain(paths: string[])}
	{#each paths as path (path)}
		{@const field = fieldAt(fields, path)}
		{#if field}<SchemaField {field} {object} {errors} idPrefix="tpl" mode="replace" />{/if}
	{/each}
{/snippet}

<fieldset class="section" {disabled}>
	<legend>Template</legend>
	{@render plain(['name', 'description', 'status'])}
</fieldset>

<fieldset class="section" {disabled}>
	<legend>Model</legend>
	<div class="field">
		<label for="tpl-model">Model&nbsp;<span class="muted">(required)</span></label>
		<select
			id="tpl-model"
			name="model"
			value={modelName}
			required
			aria-describedby="tpl-model-help"
			aria-invalid={errors['spec.model'] || errors.model ? 'true' : undefined}
			onchange={(event) => (pickedModel = event.currentTarget.value)}
		>
			{#if modelName === ''}<option value="" disabled>Choose…</option>{/if}
			{#if unknownModel}<option value={modelName}
					>{modelName} (not granted to this workspace)</option
				>{/if}
			{#each models as m (m.id)}
				<option value={m.name} disabled={m.state !== 'active' && m.name !== spec?.model}>
					{m.name}{m.state === 'active' ? '' : ` (${m.state})`}
				</option>
			{/each}
		</select>
		<p class="help" id="tpl-model-help">
			{#if models.length === 0}
				This workspace has not been granted any model. Ask an administrator of the organisation or
				the platform for a grant.
			{:else if model && model.state !== 'active'}
				This model is {model.state}{model.successor ? `; its successor is ${model.successor}` : ''}.
				New templates should choose an active model.
			{:else}
				Only the models granted to this workspace are offered. A model that is not active cannot be
				chosen for a new version.
			{/if}
		</p>
		{#if errors['spec.model'] || errors.model}
			<p class="error">{errors['spec.model'] ?? errors.model}</p>
		{/if}
	</div>
</fieldset>

<fieldset class="section" {disabled}>
	<legend>Sampling</legend>
	{#each SAMPLING as definition (definition.name)}
		<ParamRow
			{definition}
			param={paramOf(spec, definition.name)}
			note={capabilityNote(definition, model?.capabilities)}
			{disabled}
		/>
	{/each}
</fieldset>

<fieldset class="section" {disabled}>
	<legend>Reasoning</legend>
	{#each REASONING as definition (definition.name)}
		<ParamRow
			{definition}
			param={paramOf(spec, definition.name)}
			note={capabilityNote(definition, model?.capabilities)}
			suggestions={definition.name === 'reasoning.effort' ? efforts : []}
			{disabled}
		/>
	{/each}
</fieldset>

<fieldset class="section" {disabled}>
	<legend>Output and delivery</legend>
	{@render plain([
		'spec.max_output_tokens',
		'spec.timeout_seconds',
		'spec.retry_max_attempts',
		'spec.prompt_cache',
		'spec.unsupported_params'
	])}
</fieldset>

<fieldset class="section" {disabled}>
	<legend>Recording</legend>
	{@render plain(['spec.audit'])}
	<div class="field">
		<span class="label">Tags added to every call</span>
		{#if tagKeys.length === 0}
			<p class="help">
				This workspace declares no tag keys, so a template cannot add tags. Declare them in the
				workspace's settings.
			</p>
		{:else}
			<div class="tags">
				{#each tagKeys as key (key)}
					<label for="tpl-tag-{key}">{key}</label>
					<input
						id="tpl-tag-{key}"
						name="tag.{key}"
						value={spec?.tags?.[key] ?? ''}
						autocomplete="off"
						placeholder="No value: the tag is not added"
					/>
				{/each}
			</div>
			<p class="help">Only the tag keys the workspace declares can be set.</p>
		{/if}
	</div>
</fieldset>

<fieldset class="section" {disabled}>
	<legend>Proxy mode</legend>
	{@render plain(['spec.proxy', 'spec.request_stream_usage'])}
</fieldset>

<fieldset class="section" {disabled}>
	<legend>This version</legend>
	{@render plain(['comment'])}
</fieldset>

<style>
	.section {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		background: var(--surface);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	.label {
		font-size: var(--text-sm);
		font-weight: 600;
	}

	.help {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.error {
		color: var(--danger);
		font-size: var(--text-sm);
		font-weight: 600;
	}

	.tags {
		display: grid;
		grid-template-columns: minmax(6rem, max-content) minmax(0, 24rem);
		align-items: center;
		gap: var(--space-2) var(--space-3);
	}

	.tags label {
		font-family: var(--font-mono);
		font-weight: 500;
	}
</style>
