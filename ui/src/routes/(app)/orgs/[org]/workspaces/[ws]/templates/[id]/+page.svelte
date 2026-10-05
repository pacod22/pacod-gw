<script lang="ts">
	import { enhance } from '$app/forms';
	import type { Schemas } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import TemplateEditor from '#lib/components/TemplateEditor.svelte';
	import { organisationPath, workspacePath } from '#lib/nav.ts';
	import { checkFindings } from '#lib/templates.ts';
	import type { PageProps } from './$types';

	type Version = Schemas['VersionInfo'];

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const list = $derived(workspacePath(params.org, params.ws, 'templates'));
	const template = $derived(data.template);
	const writable = $derived(data.context.writable);
	const problem = $derived(
		form && !form.ok && form.action === 'save' && form.problem?.placement === 'form'
			? form.problem
			: null
	);
	const findings = $derived(data.check ? checkFindings(data.check) : []);
	const nextVersion = $derived(
		Math.max(0, ...(template?.versions ?? []).map((v) => v.version)) + 1
	);

	let busy = $state(false);
	let cloning = $state(false);
	let rollingBack = $state(false);
	let target = $state<Version | null>(null);
</script>

<div class="stack-lg editor">
	<PageHeader
		title="Template @{template?.name ?? ''}"
		description="Saving creates a new version, which calls use at once. Every version stays, and any of them can be made the current one again."
		crumbs={[
			{ label: organisation, href: organisationPath(params.org) },
			{ label: 'Templates', href: list }
		]}
	>
		{#snippet actions()}
			{#if writable && template}
				<button type="button" class="btn" onclick={() => (cloning = true)}>
					Copy to another workspace
				</button>
			{/if}
		{/snippet}
	</PageHeader>

	{#if data.created && !form}
		<Notice tone="success"><p>Template “{template?.name}” was created as version 1.</p></Notice>
	{/if}
	<OutcomeNotice outcome={form} always={['rollback']} />

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if template}
		{#if findings.length > 0}
			<Notice tone="warning" title="The current version needs attention">
				{#each findings as finding (finding)}<p>{finding}</p>{/each}
			</Notice>
		{/if}

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
			<input type="hidden" name="version" value={template.version} />
			{#if problem}<ProblemNotice {problem} />{/if}
			{#key template.current_version}
				<TemplateEditor
					{template}
					models={data.context.models}
					tagKeys={data.context.tagKeys}
					fields={data.context.fields}
					{problem}
					disabled={!writable}
				/>
			{/key}
			{#if writable}
				<div class="row">
					<button type="submit" class="btn btn-primary" disabled={busy}>
						{busy ? 'Saving…' : `Save as version ${nextVersion}`}
					</button>
					<a class="btn" href={list}>Back to the templates</a>
				</div>
			{:else}
				<p class="muted small">Your role lets you read this template but not change it.</p>
			{/if}
		</form>

		<section class="stack">
			<h2>Versions</h2>
			<p class="muted">
				Version {template.current_version} is the one calls use.
			</p>
			<DataTable
				caption="Versions"
				rows={template.versions}
				rowKey={(v) => v.version}
				columns={[
					{ key: 'version', label: 'Version', kind: 'number' },
					{
						key: 'current',
						label: '',
						value: (v) => (v.version === template.current_version ? 'Current' : '')
					},
					{ key: 'comment', label: 'What it changed', wrap: true, empty: 'No note' },
					{ key: 'created_by', label: 'Saved by', kind: 'code' },
					{ key: 'created_at', label: 'Saved (UTC)', kind: 'datetime' }
				]}
				actions={versionActions}
			/>
		</section>
	{/if}
</div>

{#snippet versionActions(version: Version)}
	<a class="btn btn-quiet" href="{list}/{params.id}/versions/{version.version}">
		View<span class="visually-hidden"> version {version.version}</span>
	</a>
	{#if writable && template && version.version !== template.current_version}
		<button
			type="button"
			class="btn btn-quiet"
			onclick={() => {
				target = version;
				rollingBack = true;
			}}
		>
			Roll back to it<span class="visually-hidden">: version {version.version}</span>
		</button>
	{/if}
{/snippet}

<ConfirmDialog
	bind:open={rollingBack}
	title="Roll back to version {target?.version ?? ''}"
	action="?/rollback"
	confirm="Roll back"
	outcome={form}
	hidden={{ target: target?.version }}
>
	<p>
		Make version <strong>{target?.version}</strong> the one calls use again? Calls use it at once. The
		versions after it stay in the list, and the next save continues the numbering.
	</p>
</ConfirmDialog>

<ActionDialog
	bind:open={cloning}
	title="Copy template @{template?.name ?? ''}"
	action="?/clone"
	submit="Copy template"
	outcome={form}
	fields={data.cloneFields}
>
	<p>
		The copy starts with the current version. It is independent afterwards: changes here do not
		reach it. The other workspace must be granted the template's model.
	</p>
</ActionDialog>

<style>
	.editor {
		max-width: 52rem;
	}

	section {
		margin-top: var(--space-4);
	}
</style>
