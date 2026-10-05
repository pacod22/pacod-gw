<script lang="ts">
	import { paramState, type ParamDefinition, type ParamValue } from '#lib/templates.ts';

	/**
	 * One parameter of a template: not set, never sent, or a value that can be locked
	 * against the caller.
	 */
	interface Props {
		definition: ParamDefinition;
		/** The parameter as the template holds it. */
		param: ParamValue<unknown> | null;
		/** What the chosen model says about the parameter, when it does not simply accept it. */
		note?: string;
		/** Values to suggest, for a text parameter. */
		suggestions?: string[];
		disabled?: boolean;
	}

	let { definition, param, note = '', suggestions = [], disabled = false }: Props = $props();
	const uid = $props.id();

	let picked = $state<string | null>(null);
	const current = $derived(picked ?? paramState(param));
	const name = $derived(`p.${definition.name}`);
	const value = $derived(
		param?.value === null || param?.value === undefined ? '' : String(param.value)
	);
</script>

<div class="param">
	<div class="head">
		<span class="label" id="{uid}-label">{definition.label}</span>
		<span class="help">{definition.help}</span>
	</div>
	<div class="controls">
		<select
			name="{name}.state"
			aria-labelledby="{uid}-label"
			value={current}
			{disabled}
			onchange={(event) => (picked = event.currentTarget.value)}
		>
			<option value="unset">Not set: the caller decides</option>
			<option value="omit">Never send it, even when the caller does</option>
			<option value="value">Set a value</option>
		</select>
		{#if current === 'value'}
			{#if definition.type === 'boolean'}
				<select name="{name}.value" aria-label="{definition.label}: value" {value} {disabled}>
					<option value="true">Yes</option>
					<option value="false">No</option>
				</select>
			{:else if definition.type === 'text'}
				<input
					type="text"
					name="{name}.value"
					aria-label="{definition.label}: value"
					{value}
					list="{uid}-suggestions"
					autocomplete="off"
					required
					{disabled}
				/>
				<datalist id="{uid}-suggestions">
					{#each suggestions as suggestion (suggestion)}<option value={suggestion}></option>{/each}
				</datalist>
			{:else}
				<input
					type="number"
					name="{name}.value"
					aria-label="{definition.label}: value"
					{value}
					min={definition.min}
					max={definition.max}
					step={definition.type === 'integer' ? 1 : 'any'}
					required
					{disabled}
				/>
			{/if}
			<label class="check">
				<input type="checkbox" name="{name}.locked" checked={param?.locked === true} {disabled} />
				<span>Locked: the caller cannot override it</span>
			</label>
		{/if}
	</div>
	{#if note}<p class="note">{note}</p>{/if}
</div>

<style>
	.param {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		padding-bottom: var(--space-3);
		border-bottom: 1px solid var(--border);
	}

	.param:last-child {
		padding-bottom: 0;
		border-bottom: 0;
	}

	.head {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-1) var(--space-3);
	}

	.label {
		font-size: var(--text-sm);
		font-weight: 600;
	}

	.help {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.controls {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2) var(--space-3);
	}

	.controls select {
		width: auto;
		min-width: 15rem;
		flex: 1 1 15rem;
	}

	.controls input:not([type='checkbox']),
	.controls select[name$='.value'] {
		width: 8rem;
		min-width: 0;
		flex: 0 0 8rem;
	}

	.check {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		font-weight: 500;
	}

	.note {
		color: var(--warning);
		font-size: var(--text-sm);
	}
</style>
