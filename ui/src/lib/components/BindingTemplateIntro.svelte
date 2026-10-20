<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import TemplateStart from './TemplateStart.svelte';

	/**
	 * The top of the form to add a plugin binding: what the template it starts from holds,
	 * what keeps it from being created as it is, and, at a workspace, the organisation's
	 * binding it would replace.
	 */
	interface Props {
		template: Schemas['BindingTemplate'];
		/** Whether the form shows the template. */
		active: boolean;
	}

	let { template, active = $bindable(true) }: Props = $props();

	const settings = $derived((template.binding.settings ?? {}) as Record<string, unknown>);
	const engine = $derived(String(settings.engine ?? 'llm'));
	const model = $derived(
		String(((settings[engine === 'jev' ? 'jev' : 'llm'] ?? {}) as { model?: unknown }).model ?? '')
	);
	const catalogue = $derived(String(settings.catalogue ?? ''));
</script>

<TemplateStart bind:active>
	The form starts from an intent binding in shadow mode: it records what it would do and changes no
	call.
	{#if catalogue}It classifies against the catalogue <code>{catalogue}</code>{/if}
	{#if model}
		with {engine === 'jev' ? 'Jev' : 'the model'} <code>{model}</code>{engine === 'llm'
			? ', the cheapest here that can classify'
			: ''}.
	{:else if catalogue}.
	{/if}
	{#if template.replaces}
		<strong>
			The organisation’s binding of the plugin, in mode {template.replaces}, applies to this
			workspace now; a binding here replaces it.
		</strong>
	{/if}
	{#if template.problems.length > 0}
		It cannot be created as it is:
		<ul>
			{#each template.problems as problem (problem)}<li>{problem}</li>{/each}
		</ul>
	{/if}
</TemplateStart>

<style>
	ul {
		margin: var(--space-1) 0 0;
		padding-left: var(--space-4);
	}
</style>
