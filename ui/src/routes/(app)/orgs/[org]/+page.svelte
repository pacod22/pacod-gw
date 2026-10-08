<script lang="ts">
	import { describeRole } from '#lib/access.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import Badge from '#lib/components/Badge.svelte';
	import LinkTiles from '#lib/components/LinkTiles.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import { organisationPath, workspacePath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, params, form }: PageProps = $props();
	let loading = $state(false);

	const org = $derived(data.organisation);
	const name = $derived(org.display_name || org.name);
	const myRoles = $derived(
		data.principal.roles.filter(
			(r) =>
				r.scope.type === 'platform' ||
				(r.scope.type === 'organisation' && r.scope.id === params.org) ||
				(r.scope.type === 'workspace' && r.organisation_id === params.org)
		)
	);
	const count = (n: number | null) => (n === null ? '–' : String(n));
	const tiles = $derived(
		[
			{
				label: 'Workspaces',
				value: String(data.workspaces.length),
				href: organisationPath(params.org, 'workspaces'),
				show: true
			},
			{
				label: 'Members',
				value: count(data.memberCount),
				note: data.pending ? `${data.pending} invited` : '',
				href: organisationPath(params.org, 'members'),
				show: data.reader
			},
			{
				label: 'Model grants',
				value: count(data.grants),
				note: data.grants === 0 ? 'No models yet' : '',
				href: organisationPath(params.org, 'grants'),
				show: data.reader
			},
			{
				label: 'Limits',
				value: count(data.limits),
				href: organisationPath(params.org, 'limits'),
				show: data.reader
			},
			{
				label: 'Routing schemes',
				value: count(data.schemes),
				href: organisationPath(params.org, 'routing'),
				show: data.reader
			}
		].filter((t) => t.show)
	);
</script>

<div class="stack-lg">
	<PageHeader
		title={name}
		description="Organisation overview: its workspaces, who works in it, and what it may use."
	>
		{#snippet actions()}
			{#if data.admin}
				<a class="btn" href={organisationPath(params.org, 'settings')}>Edit organisation</a>
				{#if data.template}
					<button type="button" class="btn" onclick={() => (loading = true)}>Load template</button>
				{/if}
				<a class="btn" href={organisationPath(params.org, 'members?invite')}>Invite people</a>
				<a class="btn btn-primary" href={organisationPath(params.org, 'workspaces?new')}
					>New workspace</a
				>
			{/if}
		{/snippet}
	</PageHeader>
	<OutcomeNotice outcome={form} />

	<div class="facts">
		<Badge value={org.status ?? 'active'} />
		<span class="muted small">Name <code>{org.name}</code></span>
		{#if data.admins.length > 0}
			<span class="muted small">Admins: {data.admins.join(', ')}</span>
		{/if}
	</div>

	{#if org.status === 'disabled'}
		<p class="notice-line">This organisation is disabled: its workspaces refuse every call.</p>
	{/if}

	<LinkTiles {tiles} />

	<section class="card stack" aria-labelledby="ws-title">
		<div class="section-head">
			<h2 id="ws-title">Workspaces</h2>
			<a href={organisationPath(params.org, 'workspaces')} class="small">All workspaces</a>
		</div>
		{#if data.workspaces.length === 0}
			<p class="muted">
				No workspaces yet.{#if data.admin}
					<a href={organisationPath(params.org, 'workspaces?new')}>Create the first one</a>, then an
					API key in it.{/if}
			</p>
		{:else}
			<ul class="ws-list">
				{#each data.workspaces as ws (ws.id)}
					<li>
						<a href={workspacePath(params.org, ws.id)}>{ws.display_name || ws.name}</a>
						<code class="muted small">{ws.name}</code>
						{#if ws.status === 'disabled'}<Badge value="disabled" />{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</section>

	{#if data.reader && data.grants === 0}
		<p class="muted">
			The organisation has no model grants yet, so its workspaces cannot call any model. Platform
			admins grant models on the <a href={organisationPath(params.org, 'grants')}>Model grants</a> page.
		</p>
	{/if}

	{#if myRoles.length > 0}
		<section class="stack-sm">
			<h2 class="eyebrow-title">Your roles here</h2>
			<ul class="roles">
				{#each myRoles as role (role.role + JSON.stringify(role.scope))}
					<li>{describeRole(role)}</li>
				{/each}
			</ul>
		</section>
	{/if}
</div>

{#if data.template}
	{@const plan = data.template}
	<ActionDialog
		bind:open={loading}
		title="Load the organisation template?"
		action="?/loadTemplate"
		submit="Load template"
		outcome={form}
		ready={plan.changes > 0}
	>
		<div class="stack">
			<p>
				The organisation template is a starting configuration: defaults the workspaces of {name}
				inherit, and routing schemes they can use. What the organisation already has is kept; change it
				on <a href={organisationPath(params.org, 'settings')}>Settings</a>.
			</p>
			<section class="stack-sm" aria-labelledby="tpl-settings">
				<h3 id="tpl-settings">Settings</h3>
				<ul class="plan">
					{#each plan.settings as step (step.key)}
						<li>
							<div class="plan-head">
								<strong>{step.label}</strong>
								{#if step.apply}<Badge value="will be set" tone="good" />{:else}<Badge
										value="kept"
										tone="neutral"
									/>{/if}
							</div>
							<p class="small">
								{#if step.apply}
									<span class="muted">{step.now ?? 'Inherited'} →</span> {step.value}
								{:else}
									{step.now} <span class="muted">(template: {step.value})</span>
								{/if}
							</p>
							{#if step.reason}<p class="muted small">{step.reason}</p>{/if}
						</li>
					{/each}
				</ul>
			</section>
			<section class="stack-sm" aria-labelledby="tpl-schemes">
				<h3 id="tpl-schemes">Routing schemes</h3>
				<ul class="plan">
					{#each plan.schemes as step (step.name)}
						<li>
							<div class="plan-head">
								<code>{step.name}</code>
								{#if step.create}<Badge value="will be created" tone="good" />{:else}<Badge
										value="exists, kept"
										tone="neutral"
									/>{/if}
							</div>
							{#if step.description}<p class="muted small">{step.description}</p>{/if}
						</li>
					{/each}
				</ul>
			</section>
			<p class="muted small">
				{plan.changes === 0
					? 'The organisation already has everything the template holds.'
					: 'Model grants and limits are not part of the template: platform admins set those.'}
			</p>
		</div>
	</ActionDialog>
{/if}

<style>
	.facts {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2) var(--space-4);
		margin-top: calc(-1 * var(--space-3));
	}

	.notice-line {
		padding: var(--space-3) var(--space-4);
		border-radius: var(--radius);
		background: var(--warning-soft);
	}

	.eyebrow-title {
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}

	.section-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--space-3);
	}

	.ws-list {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(15rem, 1fr));
		gap: var(--space-2) var(--space-4);
		padding: 0;
		list-style: none;
	}

	.ws-list li {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
	}

	.stack-sm {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.roles {
		padding-left: var(--space-5);
	}

	h3 {
		font-size: var(--text-md);
	}

	.plan {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		padding: 0;
		list-style: none;
	}

	.plan-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
	}
</style>
