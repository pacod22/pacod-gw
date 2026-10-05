<script lang="ts">
	import { enhance } from '$app/forms';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import type { Schemas } from '#lib/api/types.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const result = $derived(form?.ok && form.data ? (form.data as Schemas['ImportResult']) : null);
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);
	// Apply is offered once a check of the document found nothing the gateway refuses.
	const checked = $derived(
		form?.ok === true && form.action === 'check' && result !== null && result.refused.length === 0
	);
	let busy = $state(false);
</script>

<div class="stack-lg config">
	<PageHeader
		title="Configuration export and import"
		description="The whole configuration as one document: providers, models, prices, organisations, workspaces, templates, limits and settings. Secrets are not part of it."
	/>

	{#if !data.admin}
		<Notice tone="warning" title="Your role does not allow this"
			><p>Export and import are for platform administrators.</p></Notice
		>
	{:else}
		<section class="card stack">
			<h2>Export</h2>
			<p>Download the configuration as it is now, as a JSON file.</p>
			<div>
				<a
					class="btn btn-primary"
					href="/platform/system/config/export"
					download
					data-sveltekit-reload>Download the configuration</a
				>
			</div>
		</section>

		<section class="card stack">
			<h2>Import</h2>
			<p>
				First check the document: the gateway says what it would create and update, what is already
				as the document has it, and what it refuses. A document with any refusal is not applied at
				all, not even in part.
			</p>
			<OutcomeNotice outcome={form} always={['check', 'apply']} />
			{#if problem && problem.placement === 'form'}<ProblemNotice {problem} />{/if}
			<form
				method="POST"
				enctype="multipart/form-data"
				class="stack"
				use:enhance={() => {
					busy = true;
					return async ({ update }) => {
						await update({ reset: false });
						busy = false;
					};
				}}
			>
				<div class="field">
					<label for="config-file">Exported file</label>
					<input id="config-file" name="file" type="file" accept="application/json,.json" />
				</div>
				<div class="field">
					<label for="config-text">Or its content</label>
					<textarea id="config-text" name="document" rows="6" class="mono" spellcheck="false"
					></textarea>
				</div>
				<div class="row">
					<button type="submit" formaction="?/check" class="btn btn-primary" disabled={busy}
						>Check the document</button
					>
					<button
						type="submit"
						formaction="?/apply"
						class="btn btn-danger"
						disabled={busy || !checked}>Import it</button
					>
					{#if !checked}<span class="muted small"
							>Importing is offered after a check without refusals.</span
						>{/if}
				</div>
			</form>

			{#if result}
				<h3>{result.applied ? 'What was imported' : 'What the import would do'}</h3>
				<dl class="facts">
					<dt>Created</dt>
					<dd>
						{result.created.length}{result.created.length ? ` – ${result.created.join(', ')}` : ''}
					</dd>
					<dt>Updated</dt>
					<dd>
						{result.updated.length}{result.updated.length ? ` – ${result.updated.join(', ')}` : ''}
					</dd>
					<dt>Unchanged</dt>
					<dd>{result.unchanged}</dd>
					<dt>Refused</dt>
					<dd>{result.refused.length}</dd>
				</dl>
				{#if result.refused.length > 0}
					<Notice
						tone="error"
						title="The gateway refuses these objects, so nothing of the document is applied"
					>
						<ul>
							{#each result.refused as refusal (refusal)}<li>{refusal}</li>{/each}
						</ul>
					</Notice>
				{/if}
			{/if}
		</section>
	{/if}
</div>

<style>
	.config {
		max-width: 52rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	ul {
		padding-left: var(--space-5);
	}
</style>
