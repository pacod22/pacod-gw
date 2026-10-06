<script lang="ts">
	import { toDateTimeInput } from '#lib/schema/form.ts';

	/** The filter form of a report: a GET form, so the filters live in the URL. */
	interface Control {
		name: string;
		label: string;
		/** `time` is a date and time in UTC; `select` needs options. */
		type?: 'text' | 'time' | 'select';
		options?: { value: string; label: string }[];
		/** A select's first choice, which leaves the filter out: "All models". */
		empty?: string;
		/**
		 * Choosing applies the form at once and clears these fields first: a workspace belongs
		 * to an organisation. A name ending in `*` clears every field it starts.
		 */
		resets?: string[];
		/** Shown in place of the choices when there is nothing to choose from yet. */
		disabled?: string;
	}

	interface Props {
		controls: Control[];
		/** The current values, by name. Names without a control are carried along hidden. */
		values: Record<string, string>;
		/**
		 * Offers a filter by tag: one field per key the workspace declares, or, with `null`, a
		 * key and a value to type. Left out, the report has no tag filter.
		 */
		tagKeys?: string[] | null;
		/** A download of what the filters select, as a CSV file. */
		csv?: string;
		/** The values to choose from per tag key; a key without them is typed. */
		tagValues?: Record<string, string[]>;
	}

	let { controls, values, tagKeys, csv, tagValues }: Props = $props();

	/** The choices of a select, with the current value even when it is not among them. */
	function choicesOf(control: Control): { value: string; label: string }[] {
		const options = control.options ?? [];
		const current = values[control.name] ?? '';
		return current !== '' && !options.some((o) => o.value === current)
			? [...options, { value: current, label: current }]
			: options;
	}

	function apply(event: Event, resets: string[] | undefined) {
		const form = (event.currentTarget as HTMLSelectElement).form;
		if (!resets || !form) return;
		for (const element of form.elements) {
			if (!(element instanceof HTMLInputElement || element instanceof HTMLSelectElement)) continue;
			const cleared = resets.some((name) =>
				name.endsWith('*') ? element.name.startsWith(name.slice(0, -1)) : element.name === name
			);
			if (cleared) element.value = '';
		}
		form.requestSubmit();
	}

	const shown = $derived(new Set(controls.map((c) => c.name)));
	// Tag fields are part of the form whenever a tag filter is offered.
	const isTagField = (name: string) =>
		name.startsWith('tag.') || name === 'tag_key' || name === 'tag_value';
	const carried = $derived(
		Object.entries(values).filter(
			([name, value]) =>
				!shown.has(name) &&
				value !== '' &&
				// New filters start at the first page.
				name !== 'offset' &&
				name !== 'cursor' &&
				!(tagKeys !== undefined && isTagField(name))
		)
	);
	const time = (value: string) =>
		/^\d{4}-\d\d-\d\dT\d\d:\d\d$/.test(value) ? value : toDateTimeInput(value);
</script>

<form method="GET" class="card filters">
	{#each carried as [name, value] (name)}
		<input type="hidden" {name} {value} />
	{/each}
	{#each controls as control (control.name)}
		<div class="field">
			<label for="filter-{control.name}">{control.label}</label>
			{#if control.type === 'select'}
				<select
					id="filter-{control.name}"
					name={control.name}
					value={values[control.name] ?? ''}
					disabled={control.disabled !== undefined}
					onchange={(event) => apply(event, control.resets)}
				>
					{#if control.disabled !== undefined}
						<option value="">{control.disabled}</option>
					{:else}
						{#if control.empty !== undefined}<option value="">{control.empty}</option>{/if}
						{#each choicesOf(control) as option (option.value)}
							<option value={option.value}>{option.label}</option>
						{/each}
					{/if}
				</select>
			{:else if control.type === 'time'}
				<input
					id="filter-{control.name}"
					name={control.name}
					type="datetime-local"
					value={time(values[control.name] ?? '')}
				/>
			{:else}
				<input
					id="filter-{control.name}"
					name={control.name}
					value={values[control.name] ?? ''}
					autocomplete="off"
				/>
			{/if}
		</div>
	{/each}
	{#if tagKeys === null}
		<div class="field">
			<label for="filter-tag-key">Tag key</label>
			<input id="filter-tag-key" name="tag_key" value={values.tag_key ?? ''} autocomplete="off" />
		</div>
		<div class="field">
			<label for="filter-tag-value">Tag value</label>
			<input
				id="filter-tag-value"
				name="tag_value"
				value={values.tag_value ?? ''}
				autocomplete="off"
			/>
		</div>
	{:else if tagKeys}
		{#each tagKeys as key (key)}
			<div class="field">
				<label for="filter-tag-{key}">Tag {key}</label>
				{#if tagValues?.[key]}
					<select id="filter-tag-{key}" name="tag.{key}" value={values[`tag.${key}`] ?? ''}>
						<option value="">Any {key}</option>
						{#each choicesOf( { name: `tag.${key}`, label: key, options: tagValues[key].map( (v) => ({ value: v, label: v }) ) } ) as option (option.value)}
							<option value={option.value}>{option.label}</option>
						{/each}
					</select>
				{:else}
					<input
						id="filter-tag-{key}"
						name="tag.{key}"
						value={values[`tag.${key}`] ?? ''}
						autocomplete="off"
					/>
				{/if}
			</div>
		{/each}
	{/if}
	<div class="buttons">
		<button type="submit" class="btn btn-primary">Apply</button>
		{#if csv}<a class="btn" href={csv} download data-sveltekit-reload>Download as CSV</a>{/if}
	</div>
</form>

<style>
	.filters {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
		gap: var(--space-3);
		align-items: end;
	}

	.buttons {
		grid-column: 1 / -1;
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}
</style>
