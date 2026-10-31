<script lang="ts">
	import type { ApiProblem } from '#lib/api/errors.ts';
	import type { Schemas } from '#lib/api/types.ts';
	import { fieldAt, type Field } from '#lib/schema/form.ts';
	import type { ModelBrief } from '#lib/server/options.ts';
	import type { InheritedRouting, SchemeOption } from '#lib/server/templates.ts';
	import {
		capabilityNote,
		paramOf,
		poolOf,
		REASONING,
		SAMPLING,
		type PoolEntry
	} from '#lib/templates.ts';
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
		/** The organisation's routing schemes. */
		schemes?: SchemeOption[];
		/** The scheme a template gets when it names none. */
		inherited?: InheritedRouting;
		/** The plain fields, from the schema. */
		fields: Field[];
		problem?: ApiProblem | null;
		disabled?: boolean;
	}

	let {
		template = null,
		models,
		tagKeys,
		schemes = [],
		inherited = { scheme: null, source: null, locked: false },
		fields,
		problem = null,
		disabled = false
	}: Props = $props();

	const spec = $derived(template?.spec);
	const errors = $derived(problem?.fields ?? {});
	// The plain form's values: the template without the comment, which belongs to a version.
	const object = $derived(template ? { ...template, comment: '' } : undefined);

	// One model, or several chosen per call by a routing scheme. The editor owns the pool
	// once it has started from the template's.
	// svelte-ignore state_referenced_locally
	let mode = $state<'single' | 'pool'>((spec?.models?.length ?? 0) > 0 ? 'pool' : 'single');
	// svelte-ignore state_referenced_locally
	let pool = $state<PoolEntry[]>(
		poolOf(spec).map((e) => ({
			model: e.model,
			tier: e.tier ?? '',
			weight: e.weight ?? 1,
			priority: e.priority ?? 0
		}))
	);
	// The tiers the organisation's schemes choose: a tier no scheme names is never chosen.
	const tiers = $derived([...new Set(schemes.flatMap((s) => s.tiers))].sort());
	const inheritedLabel = $derived(
		inherited.scheme
			? `The ${inherited.source}'s: ${inherited.scheme}`
			: 'None: the models in order of priority, the next one when one fails'
	);

	function usePool() {
		if (pool.length === 0) pool = [{ model: modelName, tier: '', weight: 1, priority: 0 }];
		mode = 'pool';
	}

	let pickedModel = $state<string | null>(null);
	const modelName = $derived(
		pickedModel ?? (mode === 'pool' ? (pool[0]?.model ?? '') : (spec?.model ?? ''))
	);
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
	<legend>Models</legend>
	<div class="modes" role="radiogroup" aria-label="How many models">
		<label>
			<input
				type="radio"
				name="model_mode"
				value="single"
				checked={mode === 'single'}
				onchange={() => (mode = 'single')}
			/>
			One model
		</label>
		<label>
			<input
				type="radio"
				name="model_mode"
				value="pool"
				checked={mode === 'pool'}
				onchange={usePool}
			/>
			Several models, chosen per call
		</label>
	</div>
	{#if mode === 'single'}
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
					This model is {model.state}{model.successor
						? `; its successor is ${model.successor}`
						: ''}. New templates should choose an active model.
				{:else}
					Only the models granted to this workspace are offered. A model that is not active cannot
					be chosen for a new version.
				{/if}
			</p>
			{#if errors['spec.model'] || errors.model}
				<p class="error">{errors['spec.model'] ?? errors.model}</p>
			{/if}
		</div>
	{:else}
		<table class="pool">
			<caption class="help">
				Tiers name groups that routing schemes choose, such as small and large; the tiers offered
				are those the organisation's schemes name. The weight is a model's share of calls for the
				weighted algorithms (0: only ever a backup); a lower priority comes first for main and
				backups.
			</caption>
			<thead>
				<tr
					><th scope="col">Model</th><th scope="col">Tier</th><th scope="col">Weight</th><th
						scope="col">Priority</th
					><th></th></tr
				>
			</thead>
			<tbody>
				{#each pool as entry, i (i)}
					<tr>
						<td>
							<select
								name="pool.model"
								aria-label="Model {i + 1}"
								bind:value={entry.model}
								required
							>
								{#if entry.model === ''}<option value="" disabled>Choose…</option>{/if}
								{#if entry.model && !models.some((m) => m.name === entry.model)}
									<option value={entry.model}>{entry.model} (not granted)</option>
								{/if}
								{#each models as m (m.id)}
									<option
										value={m.name}
										disabled={m.state !== 'active' && !poolOf(spec).some((e) => e.model === m.name)}
									>
										{m.name}{m.state === 'active' ? '' : ` (${m.state})`}
									</option>
								{/each}
							</select>
						</td>
						<td>
							<select name="pool.tier" aria-label="Tier of model {i + 1}" bind:value={entry.tier}>
								<option value="">none</option>
								{#if entry.tier && !tiers.includes(entry.tier)}
									<option value={entry.tier}>{entry.tier} (no scheme chooses it)</option>
								{/if}
								{#each tiers as t (t)}
									<option value={t}>{t}</option>
								{/each}
							</select>
						</td>
						<td
							><input
								name="pool.weight"
								aria-label="Weight of model {i + 1}"
								type="number"
								min="0"
								max="10000"
								bind:value={entry.weight}
							/></td
						>
						<td
							><input
								name="pool.priority"
								aria-label="Priority of model {i + 1}"
								type="number"
								min="0"
								max="255"
								bind:value={entry.priority}
							/></td
						>
						<td>
							<button
								type="button"
								class="btn btn-quiet"
								aria-label="Remove model {i + 1}"
								onclick={() => (pool = pool.filter((_, k) => k !== i))}
								disabled={pool.length === 1}
							>
								Remove
							</button>
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
		<div>
			<button
				type="button"
				class="btn"
				onclick={() =>
					(pool = [...pool, { model: '', tier: '', weight: 1, priority: pool.length }])}
				disabled={pool.length >= 16}
			>
				Add a model
			</button>
		</div>
		<div class="field">
			<label for="tpl-scheme">Routing scheme</label>
			<select
				id="tpl-scheme"
				name="routing_scheme"
				value={spec?.routing_scheme ?? ''}
				disabled={inherited.locked && !spec?.routing_scheme}
			>
				<option value="">Inherit — {inheritedLabel}</option>
				{#each schemes as s (s.name)}
					<option value={s.name}
						>{s.name}{s.active ? '' : ' (disabled)'}{s.description
							? ` — ${s.description}`
							: ''}</option
					>
				{/each}
			</select>
			<p class="help">
				{#if inherited.locked}
					The organisation decides the routing scheme for all its templates: {inherited.scheme ??
						'none'}.
				{:else}
					The scheme decides per call which models serve first and which take over when one fails.
					Schemes belong to the organisation.
				{/if}
			</p>
			{#if errors['spec.routing_scheme'] || errors['spec.models']}
				<p class="error">{errors['spec.routing_scheme'] ?? errors['spec.models']}</p>
			{/if}
		</div>
	{/if}
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

	.modes {
		display: flex;
		gap: var(--space-4);
		flex-wrap: wrap;
	}

	.modes label {
		display: inline-flex;
		gap: var(--space-1);
		align-items: center;
	}

	.pool {
		width: 100%;
		border-collapse: collapse;
	}

	.pool caption {
		text-align: left;
		padding-bottom: var(--space-2);
	}

	.pool th {
		text-align: left;
		font-size: var(--text-sm);
	}

	.pool td {
		padding: var(--space-1) var(--space-2) var(--space-1) 0;
	}

	.pool input[type='number'] {
		width: 6rem;
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
