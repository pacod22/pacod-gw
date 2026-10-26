<script lang="ts">
	import { enhance } from '$app/forms';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import SchemaForm from '#lib/components/SchemaForm.svelte';
	import { exampleAfterSwitch } from '#lib/playground.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);
	const preview = $derived(
		form?.ok && form.action === 'transform' ? (form.data as Record<string, unknown>) : null
	);
	const answer = $derived(form?.ok && form.action === 'chat' ? form.data : null);
	const list = (value: unknown) => (Array.isArray(value) ? value.map(String) : []);
	let busy = $state(false);

	/** A new interface gets the example body in its own shape, unless the body was edited. */
	function switchExample(event: Event & { currentTarget: HTMLFormElement }) {
		const target = event.target;
		if (!(target instanceof HTMLSelectElement) || target.name !== 'interface') return;
		const request = event.currentTarget.elements.namedItem('request');
		if (!(request instanceof HTMLTextAreaElement)) return;
		const next = exampleAfterSwitch(target.value, request.value);
		if (next !== null) request.value = next;
	}
</script>

<div class="stack-lg playground">
	<PageHeader
		title="Playground of {workspace}"
		description="Try a request as this workspace. The preview shows what the gateway would send to the provider without calling it; a test call is made, counted and billed like any other call of the workspace."
	/>

	<form
		method="POST"
		class="card stack"
		onchange={switchExample}
		use:enhance={() => {
			busy = true;
			return async ({ update }) => {
				await update({ reset: false });
				busy = false;
			};
		}}
	>
		{#if problem}<ProblemNotice {problem} />{/if}
		<SchemaForm fields={data.fields} object={data.example} {problem} idPrefix="pg" mode="replace" />
		<div class="row">
			<button type="submit" formaction="?/transform" class="btn btn-primary" disabled={busy}>
				Show what would be sent
			</button>
			<button type="submit" formaction="?/chat" class="btn" disabled={busy}>Make the call</button>
		</div>
	</form>

	{#if preview}
		<section class="card stack">
			<h2>What the gateway would send</h2>
			<dl class="facts">
				<dt>Route</dt>
				<dd>{String(preview.route).replaceAll('_', ' ')}</dd>
				<dt>Provider</dt>
				<dd>{preview.provider}</dd>
				<dt>Model requested</dt>
				<dd class="mono">{preview.model_requested}</dd>
				<dt>Model served</dt>
				<dd class="mono">{preview.model_served}</dd>
				<dt>Template</dt>
				<dd>
					{preview.template ? `${preview.template}, version ${preview.template_version}` : 'None'}
				</dd>
				<dt>URL</dt>
				<dd class="mono">{preview.url}</dd>
				<dt>Streamed</dt>
				<dd>{preview.stream ? 'Yes' : 'No'}</dd>
				<dt>Left out</dt>
				<dd>{list(preview.dropped_params).join(', ') || 'Nothing'}</dd>
			</dl>
			{#each list(preview.warnings) as warning (warning)}
				<p class="warning">{warning}</p>
			{/each}
			<h3>Body</h3>
			<pre>{JSON.stringify(preview.body, null, 2)}</pre>
		</section>
	{/if}

	{#if answer !== null && answer !== undefined}
		<section class="card stack">
			<h2>The answer</h2>
			<pre>{JSON.stringify(answer, null, 2)}</pre>
		</section>
	{/if}
</div>

<style>
	.playground {
		max-width: 56rem;
	}

	pre {
		margin: 0;
		padding: var(--space-3);
		overflow-x: auto;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--surface-2);
		font-family: var(--font-mono);
		font-size: var(--text-sm);
	}

	.warning {
		color: var(--warning);
		font-size: var(--text-sm);
	}
</style>
