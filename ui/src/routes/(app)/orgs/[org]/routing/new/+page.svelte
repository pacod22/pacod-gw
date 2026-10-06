<script lang="ts">
	import { enhance } from '$app/forms';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import RoutingSchemeEditor from '#lib/components/RoutingSchemeEditor.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const list = $derived(organisationPath(params.org, 'routing'));
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);
	let busy = $state(false);
</script>

<div class="stack-lg editor">
	<PageHeader
		title="Add routing scheme"
		description="Rules choose tiers of a template's models by what the caller says about a call; the first rule that matches decides. Templates tag their models with tiers such as small and large."
		crumbs={[
			{ label: organisation, href: organisationPath(params.org) },
			{ label: 'Routing schemes', href: list }
		]}
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
			<RoutingSchemeEditor />
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
</style>
