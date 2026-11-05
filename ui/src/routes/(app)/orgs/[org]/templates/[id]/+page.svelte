<script lang="ts">
	import { enhance } from '$app/forms';
	import type { Schemas } from '#lib/api/types.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import JsonView from '#lib/components/JsonView.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import TemplateEditor from '#lib/components/TemplateEditor.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import { checkFindings } from '#lib/templates.ts';
	import type { PageProps } from './$types';

	type Version = Schemas['VersionInfo'];

	let { data, form, params }: PageProps = $props();
	const list = $derived(organisationPath(params.org, 'templates'));
	const template = $derived(data.template);
	const writable = $derived(data.context.writable);
	const problem = $derived(
		form && !form.ok && form.action === 'save' && form.problem?.placement === 'form'
			? form.problem
			: null
	);
	const findings = $derived(data.check ? checkFindings(data.check) : []);
	const versions = $derived(data.allVersions ?? template?.versions ?? []);
	const nextVersion = $derived(Math.max(0, ...versions.map((v) => v.version)) + 1);

	let busy = $state(false);
	let rollingBack = $state(false);
	let target = $state<Version | null>(null);
</script>

<div class="stack-lg editor">
	<PageHeader
		title="Template @{template?.name ?? ''}"
		description="Every workspace of the organisation inherits this template, except one with a template of the same name of its own. Saving creates a new version, which calls in those workspaces use at once. Every version stays, and any of them can be made the current one again."
		crumbs={[{ label: 'Templates', href: list }]}
	>
		{#snippet actions()}
			{#if template}
				<a class="btn" href="{list}/{params.id}/export" download="{template.name}.template.json">
					Export
				</a>
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
					scope={data.context.scope}
					models={data.context.models}
					tagKeys={data.context.tagKeys}
					schemes={data.context.schemes}
					inherited={data.context.inherited}
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
				<p class="muted small">
					Your role lets you read this template but not change it. The organisation's administrators
					change it.
				</p>
			{/if}
		</form>

		<p class="muted">
			A call is always made in a workspace. To try the template, name <code>@{template.name}</code> as
			the model in the Playground of a workspace that inherits it.
		</p>

		{#if data.check?.parameters_schema}
			<section class="stack" aria-labelledby="schema-title">
				<h2 id="schema-title">What a call may set</h2>
				<p class="muted">
					The JSON Schema applications read for <code>@{template.name}</code> from the model lists:
					what every model of the pool takes, under this version's rules. A parameter shown as
					<code>false</code> is left out of calls; a <code>readOnly</code> value is the template's, whatever
					a call sends.
				</p>
				<details class="card">
					<summary>Parameter schema</summary>
					<JsonView
						value={data.check.parameters_schema}
						what="the parameter schema of @{template.name}"
					/>
				</details>
			</section>
		{/if}

		<section class="stack">
			<h2>Versions</h2>
			<p class="muted">
				Version {template.current_version} is the one calls use.
			</p>
			<DataTable
				caption="Versions"
				rows={versions}
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
		Make version <strong>{target?.version}</strong> the one calls use again? Calls in every workspace
		that inherits it use it at once. The versions after it stay in the list, and the next save continues
		the numbering.
	</p>
</ConfirmDialog>

<style>
	.editor {
		max-width: 52rem;
	}

	section {
		margin-top: var(--space-4);
	}
</style>
