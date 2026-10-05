<script lang="ts">
	import { describeRole, hasPlatformRole } from '#lib/access.ts';
	import Badge from '#lib/components/Badge.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const status = $derived(data.status);
	const behind = $derived(status ? status.instances.length - status.instances_current : 0);
</script>

<div class="stack-lg">
	<PageHeader
		title="Dashboard"
		description="The state of the installation. Spend, requests, errors and gateway delay for the selected scope will be shown here once the analytics reports are connected."
	/>

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
				<p class="figure">{status.software_version}</p>
				<p class="muted small">
					Instance {status.instance_id}, running since {formatDateTime(status.started_at)} UTC.
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
				Choose an organisation and a workspace in the switcher of the navigation. Workspaces hold
				the API keys that applications call the gateway with; the request log under Insight shows
				what those calls did.
			</p>
			{#if data.orgs.length > 0}
				<h3>Your organisations</h3>
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
		font-size: var(--text-xs);
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.figure {
		font-size: var(--text-xl);
		font-weight: 650;
	}

	.plain {
		padding: 0;
		list-style: none;
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
