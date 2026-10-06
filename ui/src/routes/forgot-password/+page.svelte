<script lang="ts">
	import { enhance } from '$app/forms';
	import AuthCard from '#lib/components/AuthCard.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	let busy = $state(false);
</script>

<svelte:head>
	<title>Forgot password · pacod-gw</title>
</svelte:head>

<AuthCard
	title="Reset your password"
	subtitle="Enter your email address and we will send you a link to choose a new password."
>
	{#if form && 'sent' in form && form.sent}
		<div class="message" role="status">
			<p>
				If an account exists for {form.email}, a link to reset its password is on its way. The link
				works once and for a limited time.
			</p>
		</div>
	{:else}
		{#if form && 'problem' in form && form.problem}
			<div class="message error" role="alert">
				<p>{form.problem.message}</p>
				{#if form.problem.status === 503}
					<p class="hint">Ask an administrator of the console to reset your password.</p>
				{/if}
			</div>
		{/if}
		<form
			method="POST"
			use:enhance={() => {
				busy = true;
				return async ({ update }) => {
					await update({ reset: false });
					busy = false;
				};
			}}
		>
			<div class="field">
				<label for="email">Email address</label>
				<input
					id="email"
					name="email"
					type="email"
					autocomplete="username"
					placeholder="you@example.com"
					required
					value={form?.email ?? data.email}
				/>
			</div>
			<button type="submit" class="btn btn-primary btn-wide" disabled={busy}>
				{busy ? 'Sending…' : 'Send reset link'}
			</button>
		</form>
	{/if}

	{#snippet footer()}
		<a href="/login">Back to sign in</a>
	{/snippet}
</AuthCard>
