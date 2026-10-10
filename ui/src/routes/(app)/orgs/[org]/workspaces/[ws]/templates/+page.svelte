<script lang="ts">
	import { enhance } from '$app/forms';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { workspacePath } from '#lib/nav.ts';
	import { describeModels } from '#lib/templates.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);
	const base = $derived(workspacePath(params.org, params.ws, 'templates'));
</script>

<ResourcePage
	title="Templates of {workspace}"
	description="A template fixes the model and the parameters of a kind of call, so that applications name the template instead of carrying the settings. Every save is a new version; an earlier one can be made current again."
	{data}
	{form}
	columns={[
		{
			key: 'name',
			label: 'Name',
			kind: 'code',
			value: (t) => `@${t.name}`,
			href: (t) => `${base}/${t.id}`
		},
		{ key: 'description', label: 'Description', wrap: true },
		{
			key: 'model',
			label: 'Models',
			kind: 'code',
			value: (t) => describeModels(t.spec),
			wrap: true
		},
		{ key: 'current_version', label: 'Version', kind: 'number' },
		{
			key: 'last_used',
			label: 'Last used',
			value: (t) =>
				data.lastUsed[t.name]
					? formatDateTime(data.lastUsed[t.name])
					: `Not in ${data.lastUsedDays} days`,
			sortValue: (t) => data.lastUsed[t.name] ?? ''
		},
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'updated_at', label: 'Changed (UTC)', kind: 'datetime' }
	]}
	empty="This workspace has no templates yet. Add one for the first kind of call your application makes."
	deleteNote="Calls that name this template are rejected. A template that an API key's allow-list names cannot be deleted until the key no longer names it."
	createHref="{base}/new"
	editHref={(template) => `${base}/${template.id}`}
>
	<details class="import">
		<summary>Import a template</summary>
		<p class="muted">
			A file that a template's Export gave, from this installation or another. Its models are named
			by their ids and must be granted to this workspace.
		</p>
		<form method="POST" action="?/import" enctype="multipart/form-data" use:enhance>
			<label class="field">
				<span>File</span>
				<input type="file" name="file" accept="application/json,.json" />
			</label>
			<label class="field">
				<span>Name (optional)</span>
				<input type="text" name="name" placeholder="The name in the file" />
			</label>
			<button type="submit" class="btn">Import</button>
		</form>
		{#if form?.action === 'import' && !form.ok}
			<p class="error" role="alert">{form.message}</p>
		{/if}
	</details>
</ResourcePage>

<style>
	.import {
		margin-block: var(--space-4, 1rem);
	}
	.import form {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		align-items: end;
	}
</style>
