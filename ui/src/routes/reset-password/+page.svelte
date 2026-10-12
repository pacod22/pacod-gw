<script lang="ts">
	import { enhance } from '$app/forms';
	import { AuthCard } from '@pacod/ui';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	let busy = $state(false);

	const invalid = $derived(!data.token || (form && 'invalid' in form && form.invalid));
</script>

<svelte:head>
	<title>Choose a new password · pacod-gw</title>
</svelte:head>

<AuthCard
	title="Choose a new password"
	subtitle={invalid ? undefined : 'At least 12 characters. You sign in with it afterwards.'}
>
	{#if invalid}
		<div class="message error" role="alert">
			<p>This link is invalid, was already used or has expired.</p>
		</div>
		<a class="btn btn-primary btn-wide" href="/forgot-password">Ask for a new link</a>
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
			<div class="field">
				<label for="password">New password</label>
				<input
					id="password"
					name="password"
					type="password"
					autocomplete="new-password"
					minlength="12"
					required
				/>
			</div>
			<div class="field">
				<label for="repeat">The new password again</label>
				<input
					id="repeat"
					name="repeat"
					type="password"
					autocomplete="new-password"
					minlength="12"
					required
				/>
			</div>
			<button type="submit" class="btn btn-primary btn-wide" disabled={busy}>
				{busy ? 'Saving…' : 'Set new password'}
			</button>
		</form>
	{/if}

	{#snippet footer()}
		<a href="/login">Back to sign in</a>
	{/snippet}
</AuthCard>
