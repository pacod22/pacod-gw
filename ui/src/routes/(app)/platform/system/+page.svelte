<script lang="ts">
	import Badge from '#lib/components/Badge.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const status = $derived(data.status);
</script>

<div class="stack-lg">
	<PageHeader
		title="System"
		description="The running instances of the core, the configuration version each has applied, and the parts of the installation."
	/>

	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}

	{#if status}
		{#if status.problems.length > 0}
			<Notice tone="warning" title="Configuration objects that could not be used">
				<ul class="problems">
					{#each status.problems as problem (problem)}<li>{problem}</li>{/each}
				</ul>
			</Notice>
		{:else}
			<Notice tone="success"
				><p>Every configuration object is in use. No problems are reported.</p></Notice
			>
		{/if}

		<section class="card stack">
			<h2>This instance</h2>
			<dl class="facts">
				<dt>Software version</dt>
				<dd>{status.software_version}</dd>
				<dt>Instance</dt>
				<dd class="mono">{status.instance_id}</dd>
				<dt>Role</dt>
				<dd>{status.role}</dd>
				<dt>Started (UTC)</dt>
				<dd>{formatDateTime(status.started_at, true)}</dd>
				<dt>Applied configuration</dt>
				<dd>
					Version {status.config_version}
					{#if status.config_version === status.config_head}
						<span class="muted">– the newest</span>
					{:else}
						<span class="muted">– the newest is {status.config_head}</span>
					{/if}
				</dd>
				<dt>Instances up to date</dt>
				<dd>{status.instances_current} of {status.instances.length}</dd>
			</dl>
		</section>

		<section class="stack">
			<h2>Components</h2>
			<DataTable
				caption="Components"
				rows={status.components}
				rowKey={(component) => component.name}
				columns={[
					{ key: 'name', label: 'Component' },
					{ key: 'status', label: 'State', kind: 'status' },
					{ key: 'detail', label: 'Detail', empty: 'Nothing to report', wrap: true }
				]}
			/>
		</section>

		<section class="stack">
			<h2>Instances</h2>
			<DataTable
				caption="Instances"
				rows={status.instances}
				columns={[
					{ key: 'id', label: 'Instance', kind: 'code' },
					{ key: 'role', label: 'Role' },
					{ key: 'hostname', label: 'Host' },
					{ key: 'software_version', label: 'Version' },
					{ key: 'applied_version', label: 'Applied configuration', kind: 'number' },
					{ key: 'started_at', label: 'Started (UTC)', kind: 'datetime' },
					{ key: 'heartbeat_at', label: 'Last heartbeat (UTC)', kind: 'datetime' }
				]}
				cell={applied}
				custom={['applied_version']}
			/>
		</section>
	{/if}

	{#snippet applied(instance: { applied_version: number })}
		<span class="applied">
			{instance.applied_version}
			{#if status && instance.applied_version < status.config_head}<Badge
					value="behind"
					tone="warn"
				/>{/if}
		</span>
	{/snippet}

	{#if data.changes.length > 0}
		<section class="stack">
			<h2>Recent configuration changes</h2>
			<DataTable
				caption="Recent configuration changes"
				rows={data.changes}
				rowKey={(change) => change.version}
				columns={[
					{ key: 'version', label: 'Version', kind: 'number' },
					{ key: 'op', label: 'Change' },
					{ key: 'kind', label: 'Kind', value: (change) => change.kind.replaceAll('_', ' ') },
					{ key: 'object_id', label: 'Object', kind: 'code' }
				]}
			/>
			<p class="muted small">
				The newest {data.changes.length} changes. Who made them is in the
				<a href="/platform/system/audit-log">audit log</a>.
			</p>
		</section>
	{/if}
</div>

<style>
	.problems {
		padding-left: var(--space-5);
	}

	.applied {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
	}
</style>
