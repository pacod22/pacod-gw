<script lang="ts">
	import { AuthCard, Button, Notice } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { brand, takeToken } from '#lib/page.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { loadRealm } from '#lib/realm.svelte.ts';
	import PageFooter from '#lib/components/PageFooter.svelte';

	let status = $state<'working' | 'confirmed' | 'changed' | 'failed'>('working');
	let email = $state('');
	let error = $state<string | null>(null);

	// The link only opens this page; the token is used when the page posts it, so a mail
	// scanner that follows links does not use it up.
	onMount(async () => {
		loadRealm().catch(() => {});
		const token = takeToken();
		if (!token) {
			status = 'failed';
			error = m.error_link_invalid();
			return;
		}
		try {
			const r = await post<{ email: string; changed?: boolean }>('verification/confirm', { token });
			email = r.email;
			status = r.changed ? 'changed' : 'confirmed';
		} catch (x) {
			error = explain(x);
			status = 'failed';
		}
	});
</script>

<svelte:head><title>{m.verify_title()}</title></svelte:head>

<AuthCard title={m.verify_title()} brand={brand()}>
	<div class="stack">
		{#if status === 'working'}
			<p class="muted">{m.ui_loading()}</p>
		{:else if status === 'confirmed'}
			<Notice tone="success">{m.verify_confirmed({ email })}</Notice>
			<p>{m.verify_confirmed_next()}</p>
		{:else if status === 'changed'}
			<Notice tone="success">{m.verify_changed({ email })}</Notice>
			<Button variant="primary" wide href={resolve('/account')}>{m.login_to_account()}</Button>
		{:else}
			<Notice tone="error">{error}</Notice>
		{/if}
	</div>
	{#snippet footer()}<PageFooter />{/snippet}
</AuthCard>
