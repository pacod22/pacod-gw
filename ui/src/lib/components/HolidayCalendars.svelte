<script lang="ts">
	import {
		calendarsOf,
		datesOf,
		missingYears,
		rowsOf,
		yearsOf,
		type Calendar,
		type CalendarRow
	} from '#lib/holidays.ts';
	import Rich from './Rich.svelte';

	/**
	 * The holiday calendars of the platform settings: one card per calendar, its dates one per
	 * line. The form sends them, as the JSON the API takes, under `name`.
	 */
	interface Props {
		/** The name of the form control. */
		name: string;
		value: Record<string, Calendar> | null | undefined;
		help: string;
	}

	let { name, value, help }: Props = $props();
	const uid = $props.id();
	// svelte-ignore state_referenced_locally
	let rows: CalendarRow[] = $state(rowsOf(value));
	const json = $derived(JSON.stringify(calendarsOf(rows)));
	const today = new Date();
</script>

<fieldset class="calendars">
	<legend>Holiday calendars</legend>
	<p class="help"><Rich text={help} /></p>
	<input type="hidden" {name} value={json} />
	{#each rows as row, i (i)}
		{@const read = datesOf(row.dates)}
		{@const missing = missingYears(read.dates, today)}
		<div class="calendar stack">
			<div class="grid">
				<div class="field">
					<label for="{uid}-name-{i}">Name</label>
					<input
						id="{uid}-name-{i}"
						class="mono"
						bind:value={row.name}
						placeholder="cn"
						pattern="[a-z0-9_\-]+"
					/>
					<p class="help">What a provider's Holidays field names.</p>
				</div>
				<div class="field">
					<label for="{uid}-tz-{i}">Time zone</label>
					<input
						id="{uid}-tz-{i}"
						class="mono"
						bind:value={row.timeZone}
						placeholder="Asia/Shanghai"
					/>
					<p class="help">The dates are days in this zone.</p>
				</div>
			</div>
			<div class="field">
				<label for="{uid}-source-{i}">Source</label>
				<input id="{uid}-source-{i}" bind:value={row.source} />
				<p class="help">Where the dates come from, for whoever adds the next year's.</p>
			</div>
			<div class="field">
				<label for="{uid}-dates-{i}">Dates, one per line (YYYY-MM-DD)</label>
				<textarea
					id="{uid}-dates-{i}"
					class="mono"
					rows="8"
					spellcheck="false"
					bind:value={row.dates}></textarea>
				<p class="help">
					{read.dates.length === 0
						? 'No dates.'
						: `${read.dates.length} dates – ${yearsOf(read.dates)}.`} Make-up working days are not listed.
				</p>
				{#if read.invalid.length > 0}
					<p class="warn">Not dates: {read.invalid.join(', ')}</p>
				{/if}
				{#if missing.length > 0}
					<p class="warn">
						No dates for {missing.join(' and ')}: off-peak prices do not apply on that year's
						holidays until they are added.
					</p>
				{/if}
			</div>
			<div>
				<button type="button" class="btn btn-quiet danger" onclick={() => rows.splice(i, 1)}>
					Remove calendar<span class="visually-hidden"> {row.name}</span>
				</button>
			</div>
		</div>
	{/each}
	<div>
		<button
			type="button"
			class="btn"
			onclick={() => rows.push({ name: '', timeZone: '', source: '', dates: '' })}
			>Add a calendar</button
		>
	</div>
</fieldset>

<style>
	.calendars {
		margin-top: var(--space-3);
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		padding: var(--space-3);
		border: 1px solid var(--border);
		border-radius: var(--radius);
	}

	.calendar {
		padding: var(--space-3);
		border: 1px dashed var(--border-strong);
		border-radius: var(--radius);
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr));
		gap: var(--space-3);
	}

	.help {
		color: var(--text-muted);
		font-size: var(--text-sm, 0.875rem);
	}

	.warn {
		color: var(--danger);
		font-size: var(--text-sm, 0.875rem);
	}
</style>
