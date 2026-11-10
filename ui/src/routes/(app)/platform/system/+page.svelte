<script lang="ts">
	import Badge from '#lib/components/Badge.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import { formatDateTime, formatMs } from '#lib/format.ts';
	import type { Fact } from '#lib/installation.ts';
	import { formatValue } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const status = $derived(data.status);
	const figuresOf = $derived(
		new Map((data.live?.instances ?? []).map((i) => [i.id, i.figures ?? null]))
	);
	type Row = NonNullable<typeof status>['instances'][number];
	const recentConflict = (row: Row) =>
		row.conflict_at !== null &&
		row.conflict_at !== undefined &&
		Date.now() - Date.parse(row.conflict_at) < 60_000;
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

		{#if data.installation}
			{@const installation = data.installation}
			<section class="card stack">
				<h2>Addresses and settings</h2>
				<p class="muted">
					What this installation was started with, as the console and the core know it. On the host, <code
						>make urls</code
					> prints the same from the running containers.
				</p>
				{#if installation.warnings.length > 0}
					<Notice tone="warning" title="Settings to check">
						<ul class="problems">
							{#each installation.warnings as warning (warning)}<li>{warning}</li>{/each}
						</ul>
					</Notice>
				{/if}
				<dl class="facts">
					{#each installation.addresses as fact (fact.label + fact.value)}{@render factRow(
							fact
						)}{/each}
				</dl>
				{#if installation.bench}
					<h3>For a benchmark on another machine</h3>
					<p class="muted small">Plain HTTP, for the admin addresses only (BENCH=1).</p>
					<dl class="facts">
						{#each installation.bench as fact (fact.label)}{@render factRow(fact)}{/each}
					</dl>
				{/if}
				<h3>Settings</h3>
				<dl class="facts">
					{#each installation.settings as fact (fact.label)}{@render factRow(fact)}{/each}
					{#if data.loadTests}
						<dt>Load tests</dt>
						<dd>
							{#await data.loadTests}
								<span class="muted">Asking the load agents…</span>
							{:then load}
								{#if !load}
									<span class="muted">The tester did not answer.</span>
								{:else if !load.configured}
									<span class="muted">Not set up</span>
									<span class="muted small note">TESTER_LOAD_AGENTS (make up LOAD_TEST=1)</span>
								{:else}
									<span class="agents">
										{#each load.agents as agent (agent.name)}
											<span class="agent">
												<span class="mono">{agent.name}</span>
												<Badge
													value={agent.reachable ? 'reachable' : 'not reachable'}
													tone={agent.reachable ? 'good' : 'bad'}
												/>
											</span>
										{/each}
									</span>
									<span class="muted small note">
										The mock target is {load.mock_available ? 'available' : 'not set up'}.
										<a href="/platform/testing/load">Load tests</a>
									</span>
								{/if}
							{/await}
						</dd>
					{/if}
				</dl>
			</section>
		{/if}

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
			<p class="muted">
				Every instance shares the database and nothing else. Gateways serve the data plane; the
				instance with the role admin serves this console. <a href="/insight/monitoring"
					>Monitoring</a
				> shows each instance's figures in more detail.
			</p>
			<DataTable
				caption="Instances"
				rows={status.instances}
				rowKey={(i) => i.id}
				columns={[
					{ key: 'id', label: 'Instance', kind: 'code' },
					{ key: 'role', label: 'Role' },
					{ key: 'hostname', label: 'Host' },
					{ key: 'software_version', label: 'Version' },
					{ key: 'applied_version', label: 'Applied configuration', kind: 'number' },
					{
						key: 'calls',
						label: 'Calls/s',
						kind: 'number',
						value: (i: Row) => {
							const f = figuresOf.get(i.id);
							return f ? formatValue('n', f.requests_per_second) || '0' : '–';
						},
						sortValue: (i: Row) => figuresOf.get(i.id)?.requests_per_second
					},
					{
						key: 'in_flight',
						label: 'In progress',
						kind: 'number',
						value: (i: Row) => figuresOf.get(i.id)?.in_flight ?? '–'
					},
					{
						key: 'p99',
						label: 'Delay, p99',
						kind: 'number',
						value: (i: Row) => {
							const ms = figuresOf.get(i.id)?.gateway_delay_p99_ms;
							return ms === null || ms === undefined ? '–' : formatMs(ms);
						}
					},
					{ key: 'started_at', label: 'Started (UTC)', kind: 'datetime' },
					{ key: 'heartbeat_at', label: 'Last heartbeat (UTC)', kind: 'datetime' }
				]}
				cell={instanceCell}
				custom={['id', 'applied_version']}
			/>
		</section>

		{#if status.stopped_instances.length > 0}
			<section class="stack">
				<h2>Stopped reporting</h2>
				<p class="muted">
					Instances that stopped heart-beating without signing off in the last day: crashed, killed,
					or cut off from the database. An instance that was stopped on purpose is forgotten after a
					day.
				</p>
				<DataTable
					caption="Instances that stopped reporting"
					rows={status.stopped_instances}
					rowKey={(i) => i.id}
					columns={[
						{ key: 'id', label: 'Instance', kind: 'code' },
						{ key: 'role', label: 'Role' },
						{ key: 'hostname', label: 'Host' },
						{ key: 'software_version', label: 'Version' },
						{ key: 'heartbeat_at', label: 'Last heartbeat (UTC)', kind: 'datetime' }
					]}
				/>
			</section>
		{/if}
	{/if}

	{#snippet factRow(fact: Fact)}
		<dt>{fact.label}</dt>
		<dd>
			{#if !fact.value}
				<span class="muted">Not set</span>
			{:else if fact.link}
				<a class="mono" href={fact.value} target="_blank" rel="noreferrer">{fact.value}</a>
			{:else}
				<span class="mono">{fact.value}</span>
			{/if}
			{#if fact.note}<span class="muted small note">{fact.note}</span>{/if}
		</dd>
	{/snippet}

	{#snippet instanceCell(instance: Row, column: { key: string })}
		{#if column.key === 'id'}
			<span class="applied">
				<span class="mono">{instance.id}</span>
				{#if instance.id === status?.instance_id}<Badge value="this page" tone="neutral" />{/if}
				{#if recentConflict(instance)}<Badge value="id in use twice" tone="bad" />{/if}
			</span>
		{:else}
			<span class="applied">
				{instance.applied_version}
				{#if status && instance.applied_version < status.config_head}<Badge
						value="behind"
						tone="warn"
					/>{/if}
			</span>
		{/if}
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

	/* A note goes under its value, and a long address may break anywhere. */
	.note {
		display: block;
	}

	dd .mono {
		overflow-wrap: anywhere;
	}

	.agents {
		display: inline-flex;
		flex-wrap: wrap;
		gap: var(--space-3);
	}

	.agent {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
	}
</style>
