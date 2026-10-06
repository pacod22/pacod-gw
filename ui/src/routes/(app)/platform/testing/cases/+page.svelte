<script lang="ts">
	import { enhance } from '$app/forms';
	import Badge from '#lib/components/Badge.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import type { CatalogSuite } from '#lib/testing.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	let busy = $state(false);
	const problem = $derived(
		form && !form.ok && form.action === 'upload' ? (form.problem ?? null) : null
	);
</script>

<PageHeader
	title="Test cases"
	description="The suites the tester knows: the built-in ones ship with it, others are uploaded here as YAML files. docs/testing.md describes the format; any built-in suite is a working example."
/>

<OutcomeNotice outcome={form} always={['delete']} />

{#if data.problem}
	<ProblemNotice problem={data.problem} />
{:else}
	<DataTable
		caption="Test suites"
		rows={data.suites}
		rowKey={(s) => s.suite}
		columns={[
			{ key: 'title', label: 'Suite', href: (s) => `/platform/testing/cases/${s.suite}` },
			{ key: 'suite', label: 'Name', kind: 'code' },
			{ key: 'source', label: 'Source' },
			{ key: 'cases', label: 'Cases', kind: 'number', value: (s) => s.cases.length },
			{ key: 'description', label: 'What it checks', wrap: true }
		]}
		cell={source}
		custom={['source']}
		actions={remove}
	/>

	<section class="card stack upload">
		<h2>Upload a suite</h2>
		<p class="muted small">
			A suite is checked before it is kept. An uploaded suite with the name of one already uploaded
			replaces it; built-in names and case ids stay reserved.
		</p>
		{#if problem}<ProblemNotice {problem} />{/if}
		<form
			method="POST"
			action="?/upload"
			enctype="multipart/form-data"
			class="stack"
			use:enhance={() => {
				busy = true;
				return async ({ update }) => {
					await update({ reset: true });
					busy = false;
				};
			}}
		>
			<div class="field">
				<label for="suite-file">Suite file</label>
				<input id="suite-file" name="file" type="file" accept=".yaml,.yml,text/yaml" />
			</div>
			<div class="field">
				<label for="suite-text">Or its content</label>
				<textarea id="suite-text" name="text" rows="8" class="mono" spellcheck="false"></textarea>
			</div>
			<div class="row">
				<button type="submit" class="btn btn-primary" disabled={busy}>Upload</button>
			</div>
		</form>
	</section>
{/if}

{#snippet source(suite: CatalogSuite)}
	<Badge value={suite.source === 'builtin' ? 'built in' : 'uploaded'} tone="neutral" />
{/snippet}

{#snippet remove(suite: CatalogSuite)}
	{#if suite.source === 'uploaded'}
		<form method="POST" action="?/delete" use:enhance>
			<input type="hidden" name="suite" value={suite.suite} />
			<button type="submit" class="btn btn-quiet danger"
				>Remove<span class="visually-hidden"> suite {suite.suite}</span></button
			>
		</form>
	{/if}
{/snippet}

<style>
	.upload {
		max-width: 52rem;
		margin-top: var(--space-5);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
</style>
