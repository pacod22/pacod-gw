<script lang="ts">
	import { page } from '$app/state';
	import type { AuditRow } from '#lib/api/types.ts';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const actor = (row: AuditRow) => data.actors[row.actor] ?? row.actor;
	const selected = $derived(page.url.searchParams.get('organisation_id') ?? '');
</script>

<ResourcePage
	title="Audit log"
	description="What administrators and automation did, newest first: every change to the configuration, and every sign-in."
	{data}
	form={null}
	columns={[
		{ key: 'at', label: 'Time (UTC)', kind: 'datetime' },
		{ key: 'actor', label: 'Who', value: actor },
		{ key: 'action', label: 'Did', value: (row) => row.action.replaceAll('_', ' ') },
		{ key: 'kind', label: 'To a', value: (row) => row.kind.replaceAll('_', ' ') },
		{ key: 'object_id', label: 'Object', kind: 'code' },
		{ key: 'organisation_id', label: 'Organisation', kind: 'code' },
		{ key: 'workspace_id', label: 'Workspace', kind: 'code' },
		{ key: 'config_version', label: 'Config version', kind: 'number' }
	]}
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
