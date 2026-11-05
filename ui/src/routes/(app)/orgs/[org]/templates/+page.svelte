<script lang="ts">
	import { enhance } from '$app/forms';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import { describeModels } from '#lib/templates.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const base = $derived(organisationPath(params.org, 'templates'));
</script>

<ResourcePage
	title="Templates of {organisation}"
	description="A template fixes the model and the parameters of a kind of call, so that applications name the template instead of carrying the settings. Every workspace of the organisation inherits these templates. A workspace's own template of the same name applies there instead. Every save is a new version; an earlier one can be made current again."
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
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'updated_at', label: 'Changed (UTC)', kind: 'datetime' }
	]}
	empty="This organisation has no templates yet. Add one for a kind of call that every workspace makes."
	deleteNote="Calls that name this template are rejected in every workspace that inherits it, unless the workspace has a template of the same name of its own. A template that an API key of such a workspace names in its allow-list cannot be deleted until the key no longer names it."
	createHref="{base}/new"
	editHref={(template) => `${base}/${template.id}`}
>
	<details class="import">
		<summary>Import a template</summary>
		<p class="muted">
			A file that a template's Export gave, from this installation or another. Its models are named
			by their ids and must be granted to this organisation. A template that is offered on the
			broker or takes members from it needs a workspace: import it into one instead.
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
