<script lang="ts">
	import { toDateTimeInput } from '#lib/schema/form.ts';

	/** The filter form of a report: a GET form, so the filters live in the URL. */
	interface Control {
		name: string;
		label: string;
		/** `time` is a date and time in UTC; `select` needs options. */
		type?: 'text' | 'time' | 'select';
		options?: { value: string; label: string }[];
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
	}

	let { controls, values, tagKeys, csv }: Props = $props();

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
				<select id="filter-{control.name}" name={control.name} value={values[control.name] ?? ''}>
					{#each control.options ?? [] as option (option.value)}
						<option value={option.value}>{option.label}</option>
					{/each}
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
				<input
					id="filter-tag-{key}"
					name="tag.{key}"
					value={values[`tag.${key}`] ?? ''}
					autocomplete="off"
				/>
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
