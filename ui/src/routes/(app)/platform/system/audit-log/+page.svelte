<script lang="ts">
	import { page } from '$app/state';
	import type { AuditRow } from '#lib/api/types.ts';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { Column } from '#lib/components/table.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const actor = (row: AuditRow) => data.actors[row.actor] ?? row.actor;
	const selected = $derived(page.url.searchParams.get('organisation_id') ?? '');
</script>

{#snippet cell(row: AuditRow, column: Column<AuditRow>)}
	{#if column.key === 'action'}
		<span class="stacked">
			<span>{row.action.replaceAll('_', ' ')}</span>
			<span class="muted small">{row.kind.replaceAll('_', ' ')}</span>
		</span>
	{:else if row.organisation_id || row.workspace_id}
		<span class="stacked">
			{#if row.organisation_id}<code class="id" title={row.organisation_id}
					>{row.organisation_id}</code
				>{/if}
			{#if row.workspace_id}<code class="id" title={row.workspace_id}>{row.workspace_id}</code>{/if}
		</span>
	{:else}
		<span class="muted">–</span>
	{/if}
{/snippet}

<ResourcePage
	title="Audit log"
	description="What administrators and automation did, newest first: every change to the configuration, and every sign-in."
	{data}
	form={null}
	columns={[
		{ key: 'at', label: 'Time (UTC)', kind: 'datetime' },
		{ key: 'actor', label: 'Who', value: actor },
		{
			key: 'action',
			label: 'Action',
			value: (row) => `${row.action.replaceAll('_', ' ')} ${row.kind.replaceAll('_', ' ')}`
		},
		{ key: 'object_id', label: 'Object', kind: 'code' },
		{
			key: 'scope',
			label: 'Organisation / workspace',
			value: (row) => [row.organisation_id, row.workspace_id].filter(Boolean).join(' ')
		},
		{ key: 'config_version', label: 'Config version', kind: 'number' }
	]}
	{cell}
	custom={['action', 'scope']}
	empty={selected
		? 'Nothing was recorded for this organisation.'
		: 'Nothing has been recorded yet.'}
>
	<form method="GET" class="filters">
		<div class="field">
			<label for="audit-org">Organisation</label>
			<select id="audit-org" name="organisation_id" value={selected}>
				<option value="">All organisations</option>
				{#each data.orgs as org (org.id)}
					<option value={org.id}>{org.label}</option>
				{/each}
			</select>
		</div>
		<button type="submit" class="btn">Apply filter</button>
	</form>
</ResourcePage>

<style>
	.stacked {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.id {
		display: inline-block;
		max-width: 11rem;
		overflow: hidden;
		padding: 0;
		background: none;
		font-size: 0.75rem;
		text-overflow: ellipsis;
		vertical-align: bottom;
		white-space: nowrap;
	}

	.filters {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		gap: var(--space-3);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 14rem;
	}
</style>
