<script lang="ts">
	import type { ApiProblem } from '#lib/api/errors.ts';
	import type { Schemas } from '#lib/api/types.ts';
	import DataTable from './DataTable.svelte';
	import PageHeader from './PageHeader.svelte';
	import Pager from './Pager.svelte';
	import ProblemNotice from './ProblemNotice.svelte';
	import Tabs from './Tabs.svelte';
	import type { Column } from './table';
	import { workspacePath } from '#lib/nav.ts';

	type ProviderObject = Schemas['ProviderObject'];

	/**
	 * Files or message batches a workspace created at a provider through the gateway. Callers
	 * see only their own workspace's objects; this is the record of all of them, deleted ones
	 * included.
	 */
	interface Props {
		org: string;
		ws: string;
		workspace: string;
		kind: 'files' | 'batches';
		data: {
			items: ProviderObject[];
			cursor: string | null;
			nextCursor: string | null;
			keys: Record<string, string>;
			problem: ApiProblem | null;
		};
		columns: Column<ProviderObject>[];
		empty: string;
	}

	let { org, ws, workspace, kind, data, columns, empty }: Props = $props();

	const shared: Column<ProviderObject>[] = $derived([
		{ key: 'api', label: 'API', value: (o) => (o.api === 'openai' ? 'OpenAI' : 'Anthropic') },
		{
			key: 'key_id',
			label: 'Created with key',
			value: (o) => data.keys[o.key_id] ?? o.key_id
		},
		{ key: 'credential_scope', label: 'Credential' },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]);
</script>

<div class="stack-lg">
	<PageHeader
		title="Provider objects of {workspace}"
		description="Files and batches this workspace's keys created at Anthropic and OpenAI through the gateway. Each key sees only its own workspace's objects; a call that names another workspace's file is refused."
	/>
	<Tabs
		label="Provider objects"
		variant="pills"
		tabs={[
			{ label: 'Files', href: workspacePath(org, ws, 'files') },
			{ label: 'Batches', href: workspacePath(org, ws, 'batches') }
		]}
	/>
	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if data.items.length === 0 && !data.cursor}
		<div class="card muted">{empty}</div>
	{:else}
		<DataTable
			caption={kind === 'files' ? 'Files' : 'Batches'}
			rows={data.items}
			columns={[...columns, ...shared]}
			paged
		/>
		<Pager cursor={data.cursor} nextCursor={data.nextCursor} count={data.items.length} />
	{/if}
</div>
