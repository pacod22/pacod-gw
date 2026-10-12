<script lang="ts">
	import { enhance } from '$app/forms';
	import { AuthCard } from '@pacod/ui';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	let busy = $state(false);

	const invalid = $derived(!data.token || (form && 'invalid' in form && form.invalid));
</script>

<svelte:head>
	<title>Confirm your email address · pacod-gw</title>
</svelte:head>

<AuthCard
	title="Confirm your email address"
	subtitle={invalid ? undefined : 'Confirm that this address is yours, then sign in.'}
>
	{#if invalid}
		<div class="message error" role="alert">
			<p>This link is invalid, was already used or has expired.</p>
			<p class="hint">
				Sign in with your email address and password: the console then offers to send a new link.
			</p>
		</div>
		<a class="btn btn-primary btn-wide" href="/login">Go to sign in</a>
	{:else}
		{#if form && 'problem' in form && form.problem}
			<div class="message error" role="alert"><p>{form.problem.message}</p></div>
		{/if}
		<form
			method="POST"
			use:enhance={() => {
				busy = true;
				return async ({ update }) => {
					await update();
					busy = false;
				};
			}}
		>
			<input type="hidden" name="token" value={data.token} />
			<button type="submit" class="btn btn-primary btn-wide" disabled={busy}>
				{busy ? 'Confirming…' : 'Confirm my email address'}
			</button>
		</form>
	{/if}

	{#snippet footer()}
		<a href="/login">Back to sign in</a>
	{/snippet}
</AuthCard>
