<script lang="ts">
	import { enhance } from '$app/forms';
	import { describeRole, hasPlatformRole, roleLabel } from '#lib/access.ts';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import Badge from '#lib/components/Badge.svelte';
	import BarChart from '#lib/components/BarChart.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import { formatCost, formatInteger, formatMs } from '#lib/format.ts';
	import { formatValue, numberOf, type Row } from '#lib/report/format.ts';
	import { chartPoints } from '#lib/report/series.ts';
	import { scopeQuery } from '#lib/nav.ts';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const placeOf = (i: (typeof data.invitations)[number]) =>
		i.workspace_name
			? `the workspace ${i.workspace_name} of ${i.organisation_name}`
			: `the organisation ${i.organisation_name}`;

	const status = $derived(data.status);
	const unhealthy = $derived((data.health ?? []).filter((h) => h.failed > 0));
	const alertTone = (severity: string) =>
		severity === 'critical' || severity === 'error'
			? 'error'
			: severity === 'warning'
				? 'warning'
				: 'info';
	const today = $derived(((data.overview.data?.periods ?? {}) as Record<string, Row>).day ?? null);
	const month = $derived(
		((data.overview.data?.periods ?? {}) as Record<string, Row>).month ?? null
	);
	const behind = $derived(status ? status.instances.length - status.instances_current : 0);
	const serving = $derived(status ? status.instances.filter((i) => i.role !== 'admin').length : 0);
</script>

<div class="stack-lg">
	<PageHeader
		title="Dashboard"
		description="Requests, errors and cost for the selected scope, and the state of the installation."
	>
		{#snippet actions()}
			<a class="btn btn-quiet" href="/insight/usage/overview{scopeQuery(data.scope)}"
				>Download the periods as CSV</a
			>
		{/snippet}
	</PageHeader>

	<OutcomeNotice outcome={form} />

	{#if data.identity}
		<p class="muted small">
			Invitations to other organisations wait in <a href={data.identity.invitations}>your account</a
			>.
		</p>
	{/if}

	{#if data.invitations.length > 0}
		<section class="card stack" aria-labelledby="invitations-title">
			<h2 id="invitations-title">Invitations for you</h2>
			<ul class="plain invitations">
				{#each data.invitations as invitation (invitation.id)}
					<li>
						<span>
							<strong>{invitation.invited_by}</strong> invited you to {placeOf(invitation)} as
							<strong>{roleLabel(invitation.role)}</strong>.
						</span>
						<span class="row">
							<form method="POST" action="?/decline" use:enhance>
								<input type="hidden" name="id" value={invitation.id} />
								<button type="submit" class="btn">Decline</button>
							</form>
							<form method="POST" action="?/accept" use:enhance>
								<input type="hidden" name="id" value={invitation.id} />
								<input type="hidden" name="place" value={placeOf(invitation)} />
								<button type="submit" class="btn btn-primary">Accept</button>
							</form>
						</span>
					</li>
				{/each}
			</ul>
		</section>
	{/if}

	{#if data.alerts && data.alerts.length > 0}
		<section class="stack">
			<h2>Open alerts</h2>
			{#each data.alerts as alert (alert.id)}
				<Notice tone={alertTone(alert.severity)} title={alert.title}>
					<p>{alert.detail}</p>
				</Notice>
			{/each}
		</section>
	{/if}

	{#if data.overview.problem}
		<ProblemNotice problem={data.overview.problem} />
	{:else if today}
		<Figures
			figures={[
				{
					label: 'Requests today',
					value: formatValue('requests', today.requests),
					note: `${formatValue('requests', month?.requests) || '0'} this month`
				},
				{
					label: 'Errors today',
					value: formatValue('errors', today.errors) || '0',
					note:
						numberOf(today, 'requests') > 0
							? `${((numberOf(today, 'errors') / numberOf(today, 'requests')) * 100).toFixed(1)}% of the calls`
							: ''
				},
				{
					label: 'Cost today',
					value: formatCost(numberOf(today, 'cost'), data.principal.reporting_currency),
					note: `${formatCost(numberOf(month, 'cost'), data.principal.reporting_currency)} this month`
				},
				{
					label: 'Tokens today',
					value: formatInteger(numberOf(today, 'input_tokens') + numberOf(today, 'output_tokens')),
					note: 'Input and output'
				},
				...(data.delay && numberOf(data.delay, 'requests') > 0
					? [
							{
								label: 'Gateway delay today',
								value: formatMs(numberOf(data.delay, 'gateway_delay_p50_ms')),
								note: `Median; 99th percentile ${formatMs(numberOf(data.delay, 'gateway_delay_p99_ms'))}`
							}
						]
					: [])
			]}
		/>
		{#if !data.usage.problem}
			<div class="charts">
				<div class="card">
					<BarChart
						title="Requests per hour, last 24 hours"
						points={chartPoints(data.usage.data, 'requests', 'hour')}
						format={(v) => `${formatInteger(v)} requests`}
					/>
				</div>
				<div class="card">
					<BarChart
						title="Cost per hour, last 24 hours"
						points={chartPoints(data.usage.data, 'cost', 'hour')}
						format={(v) => formatCost(v, data.principal.reporting_currency)}
					/>
				</div>
			</div>
		{/if}
		{#if data.health}
			<p>
				{#if data.health.length === 0}
					Provider health: no calls were made recently, so there is nothing to judge it by.
				{:else if unhealthy.length === 0}
					Provider health: all {data.health.length} provider and model pairs with recent calls answer
					without failures.
				{:else}
					Provider health: <strong>{unhealthy.length} of {data.health.length}</strong> provider and
					model pairs had failed calls recently ({unhealthy.map((h) => h.model).join(', ')}).
				{/if}
				<a href="/insight/monitoring">Monitoring</a> has the details.
			</p>
		{/if}
		<p>
			<a href="/insight/usage{scopeQuery(data.scope)}">Usage and costs</a> has the series, the split by
			model and the filters.
		</p>
	{/if}

	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}

	{#if status}
		{#if status.problems.length > 0}
			<Notice tone="warning" title="Some configuration objects could not be used">
				<ul class="plain">
					{#each status.problems as problem (problem)}<li>{problem}</li>{/each}
				</ul>
			</Notice>
		{/if}

		<div class="tiles">
			<section class="card tile">
				<h2>Configuration</h2>
				<p class="figure">Version {status.config_head}</p>
				<p class="muted small">
					{#if behind === 0}
						Applied by {status.instances.length === 1
							? 'the one running instance'
							: `all ${status.instances.length} running instances`}.
					{:else}
						{behind} of {status.instances.length} instances have not applied it yet.
					{/if}
				</p>
			</section>
			<section class="card tile">
				<h2>Core</h2>
				<p class="figure">
					{status.instances.length}
					{status.instances.length === 1 ? 'instance' : 'instances'}
				</p>
				<p class="muted small">
					{serving} serving traffic, version {status.software_version}.
					{#if status.stopped_instances.length > 0}
						<a href="/platform/system">{status.stopped_instances.length} stopped reporting.</a>
					{/if}
				</p>
				<p class="muted small">
					This page came from {status.instance_id}, running since {formatDateTime(
						status.started_at
					)} UTC.
				</p>
			</section>
			<section class="card tile">
				<h2>Components</h2>
				<ul class="plain components">
					{#each status.components as component (component.name)}
						<li>
							<span>{component.name}</span>
							<Badge value={component.status} />
						</li>
					{/each}
				</ul>
			</section>
		</div>
		<p>
			<a href="/platform/system">Open the system page</a> for instances, recent changes and the audit
			log.
		</p>
	{:else if !data.problem}
		<section class="card stack">
			<h2>Where to start</h2>
			<p>
				You are signed in as <strong>{data.principal.display}</strong>. What you can see and change
				follows from your roles:
			</p>
			{#if data.principal.roles.length > 0}
				<ul>
					{#each data.principal.roles as role (role.role + JSON.stringify(role.scope))}
						<li>{describeRole(role)}</li>
					{/each}
				</ul>
			{:else}
				<Notice tone="warning" title="You have no role yet">
					<p>Ask a platform administrator to give you a role in an organisation or a workspace.</p>
				</Notice>
			{/if}
			<p>
				Choose an organisation and a workspace in the bar at the top. Workspaces hold the API keys
				that applications call the gateway with; the request log under Insight shows what those
				calls did.
			</p>
			{#if data.orgs.length > 0}
				<h3 id="your-organisations">Your organisations</h3>
				<ul>
					{#each data.orgs as org (org.id)}
						<li><a href={organisationPath(org.id)}>{org.label}</a></li>
					{/each}
				</ul>
			{/if}
			{#if hasPlatformRole(data.principal)}
				<p>
					Platform pages show the <a href="/platform/providers">providers</a> and
					<a href="/platform/organisations">organisations</a> of the installation.
				</p>
			{/if}
		</section>
	{/if}
</div>

<style>
	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr));
		gap: var(--space-4);
	}

	.tiles {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
		gap: var(--space-4);
	}

	.tile {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.tile h2 {
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}

	.figure {
		font-family: var(--font-display);
		font-size: 1.5rem;
		font-weight: 700;
		letter-spacing: -0.02em;
	}

	.plain {
		padding: 0;
		list-style: none;
	}

	.invitations li {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-2) var(--space-4);
		padding: var(--space-2) 0;
	}

	.invitations li + li {
		border-top: 1px solid var(--border);
	}

	.components {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.components li {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
	}

	ul:not(.plain) {
		padding-left: var(--space-5);
	}
</style>
