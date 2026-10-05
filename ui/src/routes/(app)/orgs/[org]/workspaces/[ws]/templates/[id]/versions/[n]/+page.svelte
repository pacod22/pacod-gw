<script lang="ts">
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import TemplateSummary from '#lib/components/TemplateSummary.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { workspacePath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const list = $derived(workspacePath(params.org, params.ws, 'templates'));
	const version = $derived(data.version);
	const isCurrent = $derived(version?.version === data.currentVersion);
	let rollingBack = $state(false);
</script>

<div class="stack-lg">
	<PageHeader
		title="Version {params.n} of @{data.name}"
		crumbs={[
			{ label: 'Templates', href: list },
			{ label: `@${data.name}`, href: `${list}/${params.id}` }
		]}
	>
		{#snippet actions()}
			{#if data.writable && version && !isCurrent}
				<button type="button" class="btn btn-primary" onclick={() => (rollingBack = true)}>
					Roll back to this version
				</button>
			{/if}
		{/snippet}
	</PageHeader>

	<OutcomeNotice outcome={form} always={['rollback']} />

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if version}
		<section class="card stack">
			<h2>{isCurrent ? 'This is the version calls use' : 'An earlier version'}</h2>
			<dl class="facts">
				<dt>Saved (UTC)</dt>
				<dd>{formatDateTime(version.created_at, true)}</dd>
				<dt>Saved by</dt>
				<dd class="mono">{version.created_by || '–'}</dd>
				<dt>What it changed</dt>
				<dd>{version.comment || 'No note'}</dd>
			</dl>
		</section>
		<section class="card stack">
			<h2>Configuration</h2>
			<TemplateSummary spec={version.spec} />
		</section>
	{/if}
</div>

<ConfirmDialog
	bind:open={rollingBack}
	title="Roll back to version {params.n}"
	action="?/rollback"
	confirm="Roll back"
	outcome={form}
>
	<p>
		Make version <strong>{params.n}</strong> the one calls use again? Calls use it at once. The versions
		after it stay in the list.
	</p>
</ConfirmDialog>
