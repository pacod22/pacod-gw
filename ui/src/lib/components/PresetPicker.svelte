<script lang="ts">
	import { fillForm } from '#lib/schema/fill.ts';

	/**
	 * "Start from a preset": a choice that fills the controls of the form it stands in. What
	 * it fills stays editable; nothing is sent for the picker itself.
	 */
	interface Preset {
		name: string;
		label: string;
		/** Shown once the preset is chosen. */
		notes: string;
		/** Values by field name, nested as in the request body. */
		values: Record<string, unknown>;
	}

	interface Props {
		label: string;
		help: string;
		presets: Preset[];
		/** Path below which the values are written, for a preset of one nested object. */
		prefix?: string;
		/** The preset the form's initial values came from, shown as chosen. */
		initial?: string;
	}

	let { label, help, presets, prefix = '', initial = '' }: Props = $props();
	const uid = $props.id();
	// svelte-ignore state_referenced_locally
	let chosen = $state(initial);
	const preset = $derived(presets.find((p) => p.name === chosen));

	function apply(event: Event & { currentTarget: HTMLSelectElement }) {
		chosen = event.currentTarget.value;
		const form = event.currentTarget.form;
		const picked = presets.find((p) => p.name === chosen);
		if (form && picked) fillForm(form, picked.values, prefix);
	}
</script>

{#if presets.length > 0}
	<div class="preset">
		<label for="{uid}-preset">{label}</label>
		<select id="{uid}-preset" value={chosen} onchange={apply} aria-describedby="{uid}-help">
			<option value="">No preset</option>
			{#each presets as p (p.name)}
				<option value={p.name}>{p.label}</option>
			{/each}
		</select>
		<p class="help" id="{uid}-help">{preset?.notes || help}</p>
	</div>
{/if}

<style>
	.preset {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		padding: var(--space-3);
		border: 1px dashed var(--border-strong);
		border-radius: var(--radius);
		background: var(--surface-2);
	}

	.help {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}
</style>
