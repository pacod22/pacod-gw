<script lang="ts">
	import { AuthCard, Button, Notice, PasswordField } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { brand, takeToken } from '#lib/page.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { loadRealm, realm } from '#lib/realm.svelte.ts';
	import PageFooter from '#lib/components/PageFooter.svelte';

	let token = $state<string | null>(null);
	let password = $state('');
	let done = $state(false);
	let error = $state<string | null>(null);
	let busy = $state(false);

	onMount(() => {
		token = takeToken();
		loadRealm().catch(() => {});
	});

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = null;
		try {
			await post('password-reset/confirm', { token, password });
			done = true;
		} catch (x) {
			error = explain(x);
		}
		busy = false;
	}
</script>

<svelte:head><title>{m.reset_title()}</title></svelte:head>

<AuthCard title={m.reset_title()} brand={brand()}>
	<div class="stack">
		{#if error}<Notice tone="error">{error}</Notice>{/if}
		{#if done}
			<Notice tone="success">{m.reset_done()}</Notice>
			<Button variant="primary" wide href={resolve('/account')}>{m.login_to_account()}</Button>
		{:else if token === null}
			<p>{m.error_link_invalid()}</p>
			<a href={resolve('/forgot-password')}>{m.reset_new_link()}</a>
		{:else}
			<form class="stack" onsubmit={submit}>
				<PasswordField
					label={m.field_new_password()}
					autocomplete="new-password"
					required
					minLength={realm.value?.password_min_length}
					missing={(n) => m.field_missing({ n })}
					bind:value={password}
					showLabel={m.field_show()}
					hideLabel={m.field_hide()}
				/>
				<Button type="submit" variant="primary" wide {busy}>{m.reset_submit()}</Button>
			</form>
		{/if}
	</div>
	{#snippet footer()}<PageFooter />{/snippet}
</AuthCard>
