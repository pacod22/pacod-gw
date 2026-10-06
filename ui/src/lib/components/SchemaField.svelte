<script lang="ts">
	import {
		formatMap,
		GROUP_SWITCH,
		isOptionalGroup,
		valueAt,
		initialValue,
		slugify,
		toDateTimeInput,
		type Field,
		type Option
	} from '#lib/schema/form.ts';
	import Rich from './Rich.svelte';
	import SchemaField from './SchemaField.svelte';

	/** One control of a schema-driven form. Groups and unions render their fields recursively. */
	interface Props {
		field: Field;
		/** The object whose values the form shows; none for a new object. */
		object?: unknown;
		/** Messages for single fields, by field path. */
		errors?: Record<string, string>;
		/** Keeps the ids of two forms on one page apart. */
		idPrefix: string;
		mode?: 'create' | 'replace';
	}

	let { field, object, errors = {}, idPrefix, mode = 'create' }: Props = $props();

	const id = $derived(`${idPrefix}-${field.path.replaceAll('.', '-')}`);
	const initial = $derived(initialValue(field, object));
	const error = $derived(errors[field.path]);

	function asText(value: unknown): string {
		return value === undefined || value === null ? '' : String(value);
	}

	function asList(value: unknown): string[] {
		return Array.isArray(value) ? value.map(String) : [];
	}

	// An optional group: whether it is set at all. Until the switch is used, as the object has it.
	let switched = $state<boolean | null>(null);
	const groupSet = $derived(
		switched ?? (object !== undefined && object !== null && valueAt(object, field.path) != null)
	);

	// What the user picked in a select, until then the initial value.
	let picked: string | null = $state(null);

	const initialTag = $derived.by(() => {
		if (field.kind !== 'union') return asText(initial);
		const discriminator = field.discriminator ?? 'type';
		const current =
			typeof initial === 'object' && initial !== null
				? (initial as Record<string, unknown>)[discriminator]
				: undefined;
		if (typeof current === 'string') return current;
		return field.required ? (field.variants?.[0]?.value ?? '') : '';
	});
	const selected = $derived(picked ?? initialTag);

	const options = $derived.by((): Option[] => {
		const list = field.options ?? [];
		// A stored value that is no longer offered stays selectable, so that saving keeps it.
		if (field.kind === 'enum' && selected && !list.some((o) => o.value === selected)) {
			return [...list, { value: selected, label: selected, help: '' }];
		}
		if (field.kind === 'choices') {
			const missing = asList(initial).filter((value) => !list.some((o) => o.value === value));
			return [
				...list,
				...missing.map((value) => ({ value, label: value, help: 'No longer offered.' }))
			];
		}
		return list;
	});
	const selectedHelp = $derived(
		field.kind === 'union'
			? (field.variants?.find((v) => v.value === selected)?.help ?? '')
			: (options.find((o) => o.value === selected)?.help ?? '')
	);
	const variant = $derived(field.variants?.find((v) => v.value === selected));

	/** An empty choice is offered when "nothing" is a value: `null`, or "use the default". */
	const emptyChoice = $derived(
		field.required && !field.nullable
			? null
			: field.nullable
				? 'Not set'
				: field.default === undefined
					? 'Default'
					: null
	);

	const help = $derived.by(() => {
		const parts = [field.help];
		if (field.kind === 'list') {
			parts.push(field.numeric ? 'Separate the numbers with commas.' : 'One per line.');
		}
		if (field.kind === 'map') parts.push('One `key: value` per line.');
		if (field.kind === 'text' && field.format === 'date-time') parts.push('Time in UTC.');
		if (field.kind === 'secret') {
			parts.push(
				mode === 'replace'
					? 'It is never shown. Leave it empty to keep the current value.'
					: 'It is stored encrypted and never shown again.'
			);
		}
		if (field.kind === 'json') parts.push('JSON.');
		return parts.filter(Boolean).join(' ');
	});

	/**
	 * Fills a name in from another control of the form as the user types there, until the
	 * name itself is edited.
	 */
	function followSlug(node: HTMLInputElement, from: string | undefined) {
		const source = from ? node.form?.elements.namedItem(from) : null;
		if (!(source instanceof HTMLInputElement) || node.value !== '') return;
		let edited = false;
		const follow = () => {
			if (!edited) node.value = slugify(source.value);
		};
		const stop = () => (edited = node.value !== '');
		source.addEventListener('input', follow);
		node.addEventListener('input', stop);
		return {
			destroy() {
				source.removeEventListener('input', follow);
				node.removeEventListener('input', stop);
			}
		};
	}

	const describedBy = $derived(
		[help ? `${id}-help` : '', error ? `${id}-error` : ''].filter(Boolean).join(' ') || undefined
	);
</script>

{#snippet label()}
	<label for={id}>
		{field.label}{#if field.required}&nbsp;<span class="required">(required)</span>{/if}
	</label>
{/snippet}

{#snippet notes()}
	{#if help}<p class="help" id="{id}-help"><Rich text={help} /></p>{/if}
	{#if selectedHelp}<p class="help"><Rich text={selectedHelp} /></p>{/if}
	{#if error}<p class="error" id="{id}-error">{error}</p>{/if}
{/snippet}

{#if field.kind === 'group'}
	<fieldset class="group">
		<legend>{field.label}</legend>
		{#if help}<p class="help"><Rich text={help} /></p>{/if}
		{#if isOptionalGroup(field)}
			<label class="check">
				<input
					type="checkbox"
					name="{field.path}.{GROUP_SWITCH}"
					checked={groupSet}
					onchange={(event) => (switched = event.currentTarget.checked)}
				/>
				<span
					>Set this here<span class="muted">
						– otherwise it is inherited or left at its default</span
					></span
				>
			</label>
		{/if}
		{#if !isOptionalGroup(field) || groupSet}
			{#each field.fields ?? [] as child (child.path)}
				<SchemaField field={child} {object} {errors} {idPrefix} {mode} />
			{/each}
		{/if}
	</fieldset>
{:else if field.kind === 'union'}
	<fieldset class="group">
		<legend>{field.label}</legend>
		<div class="field">
			<label for={id}>
				{field.discriminator === 'type'
					? 'Kind'
					: field.discriminator}{#if field.required}&nbsp;<span class="required">(required)</span
					>{/if}
			</label>
			<select
				{id}
				name="{field.path}.{field.discriminator}"
				value={selected}
				required={field.required}
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}
				onchange={(event) => (picked = event.currentTarget.value)}
			>
				{#if !field.required}<option value="">Not set</option>{/if}
				{#each field.variants ?? [] as v (v.value)}
					<option value={v.value}>{v.label}</option>
				{/each}
			</select>
			{@render notes()}
		</div>
		{#key selected}
			{#each variant?.fields ?? [] as child (child.path)}
				<SchemaField field={child} {object} {errors} {idPrefix} {mode} />
			{/each}
		{/key}
	</fieldset>
{:else if field.kind === 'choices'}
	<fieldset class="group choices" aria-describedby={describedBy}>
		<legend>{field.label}</legend>
		{#if help}<p class="help" id="{id}-help"><Rich text={help} /></p>{/if}
		{#each options as option (option.value)}
			<label class="check">
				<input
					type="checkbox"
					name={field.path}
					value={option.value}
					checked={asList(initial).includes(option.value)}
				/>
				<span
					>{option.label}{#if option.help}<span class="muted"> – {option.help}</span>{/if}</span
				>
			</label>
		{:else}
			<p class="help">There is nothing to choose from yet.</p>
		{/each}
		{#if error}<p class="error" id="{id}-error">{error}</p>{/if}
	</fieldset>
{:else if field.kind === 'boolean'}
	<div class="field">
		<label class="check" for={id}>
			<input
				{id}
				type="checkbox"
				name={field.path}
				checked={initial === true}
				aria-describedby={describedBy}
			/>
			<span>{field.label}</span>
		</label>
		{@render notes()}
	</div>
{:else}
	<div class="field">
		{@render label()}
		{#if field.kind === 'enum'}
			<select
				{id}
				name={field.path}
				value={selected}
				required={field.required}
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}
				onchange={(event) => (picked = event.currentTarget.value)}
			>
				{#if emptyChoice}
					<option value="">{emptyChoice}</option>
				{:else if field.required && !selected}
					<option value="" disabled>Choose…</option>
				{/if}
				{#each options as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		{:else if field.kind === 'list' && field.numeric}
			<input
				{id}
				type="text"
				name={field.path}
				value={asList(initial).join(', ')}
				inputmode="numeric"
				autocomplete="off"
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}
			/>
		{:else if field.kind === 'map'}
			<textarea
				{id}
				name={field.path}
				rows="3"
				spellcheck="false"
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}>{formatMap(initial)}</textarea
			>
		{:else if field.kind === 'list'}
			<textarea
				{id}
				name={field.path}
				rows="3"
				spellcheck="false"
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}>{asList(initial).join('\n')}</textarea
			>
		{:else if field.kind === 'json'}
			<textarea
				{id}
				name={field.path}
				class="mono"
				rows="4"
				spellcheck="false"
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}
				>{initial === undefined || initial === null
					? ''
					: JSON.stringify(initial, null, 2)}</textarea
			>
		{:else if field.kind === 'number'}
			<input
				{id}
				type="number"
				name={field.path}
				value={asText(initial)}
				step={field.integer ? 1 : 'any'}
				min={field.min}
				max={field.max}
				inputmode={field.integer ? 'numeric' : 'decimal'}
				required={field.required}
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}
			/>
		{:else if field.kind === 'secret'}
			<input
				{id}
				type="password"
				name={field.path}
				autocomplete="new-password"
				required={field.required}
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}
			/>
		{:else if field.format === 'date-time'}
			<input
				{id}
				type="datetime-local"
				name={field.path}
				value={toDateTimeInput(initial)}
				required={field.required}
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}
			/>
		{:else}
			<input
				{id}
				type={field.format === 'email' ? 'email' : 'text'}
				name={field.path}
				value={asText(initial)}
				use:followSlug={mode === 'create' ? field.slugFrom : undefined}
				autocomplete="off"
				autocapitalize="off"
				spellcheck="false"
				required={field.required}
				aria-describedby={describedBy}
				aria-invalid={error ? 'true' : undefined}
			/>
		{/if}
		{@render notes()}
	</div>
{/if}

<style>
	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}

	.group {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.choices {
		gap: var(--space-2);
	}

	.check {
		display: flex;
		align-items: baseline;
		gap: var(--space-2);
		font-weight: 500;
	}

	.check input {
		flex: none;
		position: relative;
		top: 0.15rem;
	}

	.required {
		color: var(--text-muted);
		font-weight: 400;
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
</style>
