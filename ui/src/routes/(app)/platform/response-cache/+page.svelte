<script lang="ts">
	import Badge from '#lib/components/Badge.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import Meter from '#lib/components/Meter.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import { formatInteger } from '#lib/format.ts';
	import { formatUnit } from '#lib/metrics/series.ts';
	import { cacheSettingsText } from '#lib/plugins.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	let clearOpen = $state(false);
	const status = $derived(data.status);
	const bytes = (n: number | null | undefined) =>
		n === null || n === undefined ? '–' : formatUnit(n, 'bytes');

	type Instance = NonNullable<typeof status>['instances'][number];
	type Clear = NonNullable<typeof status>['recent_clears'][number];
	// A management-only instance's own store holds nothing: its figures would only mislead.
	const serving = $derived((status?.instances ?? []).filter((i) => i.serves_calls));
	const sum = (rows: Instance[], key: 'entries' | 'bytes' | 'capacity_bytes') =>
		rows.reduce((total, i) => total + (i.figures?.[key] ?? 0), 0);

	const STORES: Record<string, string> = {
		valkey: 'Valkey, shared by every gateway',
		memory: 'The memory of each gateway',
		mixed: 'Valkey for some gateways, their memory for others',
		unknown: 'Not reported yet'
	};

	const figures = $derived.by(() => {
		if (!status) return [];
		const valkey = status.valkey;
		const breakers = serving.filter((i) => i.figures?.breaker_open).length;
		return [
			{ label: 'Store', value: STORES[status.store] ?? status.store },
			valkey
				? {
						label: 'Answers kept',
						value: valkey.reachable ? formatInteger(valkey.keys) : '–',
						note: 'Keys in Valkey'
					}
				: {
						label: 'Answers kept',
						value: formatInteger(sum(serving, 'entries')),
						note: `In ${serving.length} ${serving.length === 1 ? 'gateway' : 'gateways'}; one answer may be in several`
					},
			valkey
				? {
						label: 'Memory used',
						value: valkey.reachable ? bytes(valkey.used_memory_bytes) : '–',
						note: valkey.max_memory_bytes ? `of ${bytes(valkey.max_memory_bytes)}` : 'No limit set'
					}
				: {
						label: 'Memory used',
						value: bytes(sum(serving, 'bytes')),
						note: `of ${bytes(sum(serving, 'capacity_bytes'))}`
					},
			{
				label: 'Breakers open',
				value: String(breakers),
				note: breakers
					? 'Those gateways skip the store for a while'
					: 'Every gateway uses the store'
			}
		];
	});

	const scopeHref = (c: Clear) =>
		c.workspace_id && c.organisation_id
			? `/orgs/${c.organisation_id}/workspaces/${c.workspace_id}/plugins`
			: c.organisation_id
				? `/orgs/${c.organisation_id}/plugins`
				: null;
</script>

<div class="stack-lg">
	<PageHeader
		title="Response cache"
		description="Where the gateways keep answers to repeated calls, what each holds, the platform’s binding, and the clears of every scope."
	/>

	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}
	<OutcomeNotice outcome={form} always={['clearCache']} />

	{#if status}
		<Figures {figures} />
		<p class="muted">
			What the cache answered and saved is under <a href="/insight/cache"
				>Insight › Usage › Response cache</a
			>; lookups and errors over time are on the
			<a href="/insight/metrics/internals">Gateway internals</a> board.
		</p>

		{#if status.valkey}
			<section class="card stack">
				<h2>Valkey</h2>
				{#if status.valkey.reachable}
					<dl class="facts">
						<dt>Keys</dt>
						<dd>{formatInteger(status.valkey.keys)}</dd>
						<dt>When full</dt>
						<dd>
							{status.valkey.eviction_policy === 'allkeys-lru'
								? 'the answers used least recently go (allkeys-lru)'
								: status.valkey.eviction_policy}
						</dd>
					</dl>
					{#if status.valkey.max_memory_bytes}
						<Meter
							used={status.valkey.used_memory_bytes ?? 0}
							amount={status.valkey.max_memory_bytes}
							label="{bytes(status.valkey.used_memory_bytes)} of {bytes(
								status.valkey.max_memory_bytes
							)}"
						/>
					{/if}
				{:else}
					<Notice tone="warning" title="Valkey does not answer">
						<p>
							{status.valkey.error ?? 'No answer.'} Calls are served by their providers meanwhile; the
							runbook's section on a failing response cache says what to check.
						</p>
					</Notice>
				{/if}
			</section>
		{:else if status.valkey_note}
			<Notice tone="info"><p>{status.valkey_note}.</p></Notice>
		{/if}

		<section class="stack">
			<h2>Instances</h2>
			<DataTable
				caption="The store of each instance"
				rows={status.instances}
				rowKey={(i) => i.id}
				columns={[
					{ key: 'id', label: 'Instance', kind: 'code' },
					{ key: 'role', label: 'Role' },
					{ key: 'hostname', label: 'Host' },
					{
						key: 'store',
						label: 'Store',
						value: (i: Instance) => (i.serves_calls ? (i.figures?.store ?? '–') : 'serves no calls')
					},
					{
						key: 'entries',
						label: 'Answers',
						kind: 'number',
						value: (i: Instance) => i.figures?.entries ?? '–'
					},
					{
						key: 'bytes',
						label: 'Memory',
						kind: 'number',
						value: (i: Instance) =>
							i.figures?.bytes === null || i.figures?.bytes === undefined
								? '–'
								: `${bytes(i.figures.bytes)} of ${bytes(i.figures.capacity_bytes)}`,
						sortValue: (i: Instance) => i.figures?.bytes
					},
					{ key: 'breaker', label: 'Store breaker' }
				]}
				cell={breakerCell}
				custom={['breaker']}
			/>
			<p class="muted small">
				In Valkey the answers are shared, and each instance shows only its breaker. An instance of
				an older release reports no figures.
			</p>
		</section>

		<section class="card stack">
			<h2>The platform’s binding</h2>
			{#if !status.install.installed}
				<p>
					The response cache is not installed: no workspace has one.
					<a href="/platform/plugins">Plugin installs</a> installs it.
				</p>
			{:else}
				<dl class="facts">
					<dt>Install</dt>
					<dd>
						<Badge
							value={status.install.enabled ? 'on' : 'switched off'}
							tone={status.install.enabled ? 'good' : 'neutral'}
						/>
					</dd>
					{#if status.install.default_binding}
						<dt>Mode</dt>
						<dd>{status.install.default_binding.mode}</dd>
						<dt>Keeps</dt>
						<dd>{cacheSettingsText(status.install.default_binding.settings)}</dd>
					{:else}
						<dt>Binding</dt>
						<dd>None: only organisations and workspaces that bind it have a cache</dd>
					{/if}
				</dl>
				<p class="muted">
					Every workspace without a binding of its own or of its organisation gets this one. It is
					changed in <a href="/platform/plugins">Plugin installs</a>.
				</p>
			{/if}
		</section>

		<section class="stack">
			<div class="head">
				<h2>Clears</h2>
				{#if data.canClear}
					<button type="button" class="btn btn-danger" onclick={() => (clearOpen = true)}
						>Clear every workspace</button
					>
				{/if}
			</div>
			<p class="muted">
				A clear makes the answers kept before it misses, in every instance. Organisation and
				workspace admins clear their own on their Plugins page.
			</p>
			{#if status.recent_clears.length > 0}
				<DataTable
					caption="Recent clears"
					rows={status.recent_clears}
					rowKey={(c) => `${c.scope}:${c.organisation_id ?? ''}:${c.workspace_id ?? ''}`}
					columns={[
						{ key: 'scope', label: 'Scope' },
						{
							key: 'name',
							label: 'Of',
							value: (c: Clear) => (c.scope === 'platform' ? 'every workspace' : (c.name ?? '–')),
							href: scopeHref
						},
						{ key: 'at', label: 'Cleared (UTC)', kind: 'datetime' },
						{ key: 'by', label: 'By', value: (c: Clear) => c.by_display ?? c.by }
					]}
				/>
			{:else}
				<p>Nothing was cleared yet.</p>
			{/if}
		</section>
	{/if}
</div>

{#snippet breakerCell(i: Instance)}
	{#if !i.figures}
		–
	{:else if i.figures.breaker_open}
		<Badge value="open" tone="bad" />
	{:else if i.figures.failures_in_a_row > 0}
		<Badge value="{i.figures.failures_in_a_row} failed" tone="warn" />
	{:else}
		<Badge value="closed" tone="good" />
	{/if}
{/snippet}

{#if data.canClear}
	<ConfirmDialog
		bind:open={clearOpen}
		title="Clear the response cache of every workspace?"
		action="?/clearCache"
		confirm="Clear every workspace"
		outcome={form}
	>
		<p>
			The answers kept for every workspace are no longer used. Calls are answered by their
			providers, and cost what they cost, until their answers are kept again.
		</p>
	</ConfirmDialog>
{/if}

<style>
	.head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--space-3);
		flex-wrap: wrap;
	}
</style>
