<script lang="ts">
	import { onMount } from 'svelte';
	import { Notice } from '@pacod/ui';
	import { explain } from '#lib/errors.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { loadMe, me } from '#lib/me.svelte.ts';
	import { loadRealm } from '#lib/realm.svelte.ts';
	import AccountShell from '#lib/components/AccountShell.svelte';

	let { children } = $props();
	let error = $state<string | null>(null);

	onMount(async () => {
		loadRealm().catch(() => {});
		try {
			await loadMe();
		} catch (e) {
			error = explain(e);
		}
	});
</script>

<AccountShell>
	{#if error}
		<Notice tone="error">{error}</Notice>
	{:else if me.value}
		{@render children()}
	{:else}
		<p class="muted">{m.ui_loading()}</p>
	{/if}
</AccountShell>
