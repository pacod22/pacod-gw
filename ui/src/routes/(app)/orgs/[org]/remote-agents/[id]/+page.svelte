<script lang="ts">
	import { enhance } from '$app/forms';
	import Badge from '#lib/components/Badge.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import { formatValue } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const agent = $derived(data.agent);
	const card = $derived(data.card);
	const base = $derived(organisationPath(params.org, 'remote-agents'));
	const TONES: Record<string, 'good' | 'bad' | 'warn' | 'neutral'> = {
		approved: 'good',
		new: 'neutral',
		changed: 'warn',
		disabled: 'bad'
	};
	const pretty = (v: unknown) => JSON.stringify(v, null, 2);
</script>

<div class="stack-lg">
	<PageHeader
		title="Remote agent {agent.name}"
		description={agent.description || 'An A2A agent of the organisation.'}
		crumbs={[{ label: 'Remote agents', href: base }]}
	>
		{#snippet actions()}
			{#if data.mayManage}
				<form method="POST" action="?/probe" use:enhance>
					<button type="submit" class="btn">Read its card now</button>
				</form>
			{/if}
		{/snippet}
	</PageHeader>
	<OutcomeNotice outcome={form} />

	<section class="card stack" aria-labelledby="card-title">
		<h2 id="card-title">Card</h2>
		<p class="muted">
			Callers reach the agent at <code>/a2a/{agent.name}</code> once its card is approved, with a
			tool bundle entry <code>a2a:{agent.name}</code>. They are served the approved card, pointed at
			the gateway. A card that changes holds the agent until it is approved again.
		</p>
		{#if card}
			<p>
				<Badge value={card.status} tone={TONES[card.status]} />
				{#if card.probe}
					<span class="muted small"
						>Read {formatValue('probed_at', card.probe.probed_at)}{card.probe.ok
							? ''
							: `: ${card.probe.problem}`}</span
					>
				{/if}
			</p>
			{#if card.seen}
				<p>Called at <code>{card.seen.interface_url}</code></p>
				<details>
					<summary>The card as the agent serves it</summary>
					<pre>{pretty(card.seen.card)}</pre>
				</details>
				{#if card.status !== 'approved' && data.mayManage}
					<form method="POST" action="?/approve" use:enhance>
						<input type="hidden" name="hash" value={card.seen.hash} />
						<button type="submit" class="btn btn-primary">Approve this card</button>
					</form>
				{/if}
			{/if}
			{#if card.pin && card.status === 'changed'}
				<details>
					<summary>The card as approved</summary>
					<pre>{pretty(card.pin.card)}</pre>
				</details>
			{/if}
		{/if}
	</section>

	<section class="card stack" aria-labelledby="credentials-title">
		<h2 id="credentials-title">Credentials</h2>
		<p class="muted">
			Sent in <code>{agent.auth.header}</code> on every request. A workspace's own credential is used
			for its callers instead of the organisation's. Secrets are never shown again.
		</p>
		{#each data.credentials as c (c.id)}
			<div class="inline-form">
				<span>{c.label || c.fingerprint}</span>
				<Badge value={c.state} />
				{#if data.mayManage}
					<form method="POST" action="?/deleteCredential" use:enhance>
						<input type="hidden" name="cid" value={c.id} />
						<button type="submit" class="btn btn-danger">Delete</button>
					</form>
				{/if}
			</div>
		{/each}
		{#if data.mayManage}
			<form method="POST" action="?/addCredential" use:enhance class="stack-sm">
				<label>Secret <input name="secret" type="password" autocomplete="off" required /></label>
				<label>Label <input name="label" /></label>
				<label
					>For one workspace only (its id; empty: the organisation) <input
						name="workspace_id"
					/></label
				>
				<button type="submit" class="btn btn-primary">Add credential</button>
			</form>
		{/if}
	</section>
</div>
