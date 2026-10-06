<script lang="ts">
	import { enhance } from '$app/forms';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import RoutingSchemeEditor from '#lib/components/RoutingSchemeEditor.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import { describeAction, describeWhen } from '#lib/routing.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();
	const list = $derived(organisationPath(params.org, 'routing'));
	const scheme = $derived(data.scheme);
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);
	let busy = $state(false);
</script>

<div class="stack-lg editor">
	<PageHeader
		title="Routing scheme {scheme?.name ?? ''}"
		description="The first rule whose conditions hold decides which tiers of a template's models serve a call, and how the first is picked. A change applies to the next call."
		crumbs={[{ label: 'Routing schemes', href: list }]}
	/>

	{#if data.created && !form}
		<Notice tone="success"><p>Routing scheme “{scheme?.name}” was created.</p></Notice>
	{/if}
	<OutcomeNotice outcome={form} />

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if scheme}
		<section class="stack" aria-label="In short">
			<ol class="summary">
				{#each scheme.rules as rule (rule.name)}
					<li><code>{rule.name}</code>: {describeWhen(rule)} → {describeAction(rule.then)}</li>
				{/each}
				<li><em>otherwise</em> → {describeAction(scheme.default)}</li>
			</ol>
		</section>
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
			<input type="hidden" name="version" value={scheme.version} />
			{#if problem}<ProblemNotice {problem} />{/if}
			{#key scheme.version}
				<RoutingSchemeEditor {scheme} disabled={!data.writable} />
			{/key}
			{#if data.writable}
				<div class="row">
					<button type="submit" class="btn btn-primary" disabled={busy}>
						{busy ? 'Saving…' : 'Save scheme'}
					</button>
					<a class="btn" href={list}>Back to the schemes</a>
				</div>
			{:else}
				<p class="muted small">Your role lets you read this scheme but not change it.</p>
			{/if}
		</form>
	{/if}
</div>

<style>
	.editor {
		max-width: 56rem;
	}

	.summary {
		margin: 0;
		padding-left: var(--space-5);
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
</style>
