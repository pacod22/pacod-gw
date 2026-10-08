<script lang="ts">
	import { enhance } from '$app/forms';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import RoutingSchemeEditor from '#lib/components/RoutingSchemeEditor.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();
	const list = $derived(organisationPath(params.org, 'routing'));
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);
	let busy = $state(false);
	const uid = $props.id();
	/** The default scheme the editor starts from; none for an empty one. */
	let start = $state('');
	const startScheme = $derived(data.defaults.find((d) => d.name === start) ?? null);
</script>

<div class="stack-lg editor">
	<PageHeader
		title="Add routing scheme"
		description="Rules choose tiers of a template's models by what the caller says about a call; the first rule that matches decides. Templates tag their models with tiers such as small and large."
		crumbs={[{ label: 'Routing schemes', href: list }]}
	/>
	{#if !data.writable}
		<p class="muted">Routing schemes are created by administrators of the organisation.</p>
	{:else}
		<form
			method="POST"
			action="?/save"
			class="stack"
			use:enhance={() => {
				busy = true;
				return async ({ update }) => {
					await update({ reset: false });
					busy = false;
				};
			}}
		>
			{#if problem}<ProblemNotice {problem} />{/if}
			{#if data.defaults.length > 0}
				<div class="start">
					<label for="{uid}-start">Start from a default scheme</label>
					<select id="{uid}-start" bind:value={start} aria-describedby="{uid}-start-help">
						<option value="">No default: an empty scheme</option>
						{#each data.defaults as d (d.name)}
							<option value={d.name}>{d.name}</option>
						{/each}
					</select>
					<p class="help" id="{uid}-start-help">
						{startScheme?.description ||
							'Choosing one fills the form with a ready-made scheme, replacing what it holds. Everything stays editable; a name that is taken must be changed.'}
					</p>
				</div>
			{/if}
			<!-- The editor copies the scheme it starts from: a new start gives a new editor. -->
			{#key start}
				<RoutingSchemeEditor scheme={startScheme} />
			{/key}
			<div class="row">
				<button type="submit" class="btn btn-primary" disabled={busy}>
					{busy ? 'Creating…' : 'Create scheme'}
				</button>
				<a class="btn" href={list}>Cancel</a>
			</div>
		</form>
	{/if}
</div>

<style>
	.editor {
		max-width: 56rem;
	}

	.start {
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
