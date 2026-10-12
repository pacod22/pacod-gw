<script lang="ts">
	import { AuthCard, Button, Notice } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { brand, go } from '#lib/page.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { loadRealm } from '#lib/realm.svelte.ts';
	import PageFooter from '#lib/components/PageFooter.svelte';

	const q = page.url.searchParams;
	let done = $state(false);
	let error = $state<string | null>(null);
	let busy = $state(false);

	onMount(() => void loadRealm().catch(() => {}));

	async function signOut() {
		busy = true;
		error = null;
		try {
			const r = await post<{ next: string; to?: string }>('logout', {
				client_id: q.get('client_id') ?? undefined,
				post_logout_redirect_uri: q.get('post_logout_redirect_uri') ?? undefined,
				state: q.get('state') ?? undefined
			});
			if (r.next === 'redirect' && r.to) return go(r.to);
			done = true;
		} catch (x) {
			error = explain(x);
		}
		busy = false;
	}
</script>

<svelte:head><title>{m.logout_title()}</title></svelte:head>

<AuthCard title={done ? m.logout_done_title() : m.logout_title()} brand={brand()}>
	<div class="stack">
		{#if error}<Notice tone="error">{error}</Notice>{/if}
		{#if done}
			<p>{m.logout_done()}</p>
		{:else}
			<p>{m.logout_intro()}</p>
			<div class="stack">
				<Button variant="primary" wide {busy} onclick={signOut}>{m.logout_submit()}</Button>
				<Button wide variant="secondary" onclick={() => history.back()}>{m.ui_cancel()}</Button>
			</div>
		{/if}
	</div>
	{#snippet footer()}<PageFooter />{/snippet}
</AuthCard>
