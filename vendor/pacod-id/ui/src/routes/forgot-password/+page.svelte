<script lang="ts">
	import { AuthCard, Button, Notice, TextField } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { brand } from '#lib/page.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { loadRealm } from '#lib/realm.svelte.ts';
	import PageFooter from '#lib/components/PageFooter.svelte';

	let email = $state('');
	let sent = $state(false);
	let error = $state<string | null>(null);
	let busy = $state(false);

	onMount(() => void loadRealm().catch(() => {}));

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = null;
		try {
			await post('password-reset', { email });
			sent = true;
		} catch (x) {
			error = explain(x);
		}
		busy = false;
	}
</script>

<svelte:head><title>{m.forgot_title()}</title></svelte:head>

<AuthCard title={m.forgot_title()} brand={brand()}>
	<div class="stack">
		{#if error}<Notice tone="error">{error}</Notice>{/if}
		{#if sent}
			<Notice tone="success">{m.forgot_sent({ email })}</Notice>
		{:else}
			<p>{m.forgot_intro()}</p>
			<form class="stack" onsubmit={submit}>
				<TextField
					label={m.field_email()}
					type="email"
					name="email"
					autocomplete="email"
					required
					bind:value={email}
				/>
				<Button type="submit" variant="primary" wide {busy}>{m.forgot_submit()}</Button>
			</form>
		{/if}
	</div>
	{#snippet footer()}<PageFooter />{/snippet}
</AuthCard>
