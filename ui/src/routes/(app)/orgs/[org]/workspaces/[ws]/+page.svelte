<script lang="ts">
	import Badge from '#lib/components/Badge.svelte';
	import LinkTiles from '#lib/components/LinkTiles.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import { scopeQuery, workspacePath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();

	const ws = $derived(data.workspace);
	const name = $derived(ws.display_name || ws.name);
	const count = (n: number | null) => (n === null ? '–' : String(n));
	const page = (p: string) => workspacePath(params.org, params.ws, p);
	const tiles = $derived([
		{
			label: 'API keys',
			value: count(data.keys?.active ?? null),
			note: data.keys?.revoked ? `${data.keys.revoked} revoked` : 'Active',
			href: page('keys')
		},
		{ label: 'Templates', value: count(data.templates), href: page('templates') },
		data.orgGrants === 0
			? {
					label: 'Model grants',
					value: 'None',
					note: 'The organisation has no models yet',
					href: page('grants')
				}
			: {
					label: 'Model grants',
					value: data.grants === 0 ? 'All' : count(data.grants),
					note:
						data.grants !== 0
							? 'Granted to this workspace'
							: data.orgGrants === null
								? 'Every model of the organisation'
								: `All ${data.orgGrants} of the organisation`,
					href: page('grants')
				},
		{ label: 'Limits', value: count(data.limits), href: page('limits') },
		{
			label: 'Members',
			value: count(data.members),
			note: 'With a role here',
			href: page('members')
		}
	]);
	const usage = $derived(`/insight/usage${scopeQuery({ org: params.org, ws: params.ws })}`);
	const requests = $derived(`/insight/requests${scopeQuery({ org: params.org, ws: params.ws })}`);
</script>

<div class="stack-lg">
	<PageHeader
		title={name}
		description="Workspace overview: what applications in it can call, and who looks after it."
	>
		{#snippet actions()}
			<a class="btn" href={page('playground')}>Open playground</a>
			{#if data.admin}
				<a class="btn btn-primary" href={page('keys?new')}>New API key</a>
			{/if}
		{/snippet}
	</PageHeader>

	<div class="facts">
		<Badge value={ws.status ?? 'active'} />
		<span class="muted small">Name <code>{ws.name}</code></span>
		<span class="muted small">Time zone {ws.time_zone}</span>
		{#if ws.tag_keys && ws.tag_keys.length > 0}
			<span class="muted small">Tag keys {ws.tag_keys.join(', ')}</span>
		{/if}
	</div>

	{#if ws.status === 'disabled'}
		<p class="notice-line">This workspace is disabled: calls with its API keys are refused.</p>
	{/if}

	<LinkTiles {tiles} />

	{#if data.keys && data.keys.active === 0}
		<section class="card stack">
			<h2>Get started</h2>
			<ol class="steps">
				<li>
					{#if data.admin}<a href={page('keys?new')}>Create an API key</a>{:else}An admin creates an
						API key{/if} for the application.
				</li>
				<li>
					Point the application at the gateway with that key, or try a call in the <a
						href={page('playground')}>playground</a
					>.
				</li>
				<li>Follow the calls in the <a href={requests}>request log</a>.</li>
			</ol>
		</section>
	{/if}

	<p>
		<a href={usage}>Usage and costs</a> and the <a href={requests}>request log</a> show what this
		workspace's calls did. <a href={page('settings')}>Settings</a> hold its name, time zone, tag keys
		and audit mode.
	</p>
</div>

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

	.steps {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		padding-left: var(--space-5);
	}
</style>
