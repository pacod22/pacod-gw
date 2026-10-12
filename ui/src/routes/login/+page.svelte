<script lang="ts">
	import { untrack } from 'svelte';
	import { enhance } from '$app/forms';
	import { AuthCard } from '@pacod/ui';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	let busy = $state(false);

	const REASONS: Record<string, string> = {
		'password-changed': 'Your password was changed. Sign in with the new password.',
		'password-reset': 'Your new password is set. Sign in with it.',
		'email-verified': 'Your email address is confirmed. You can sign in now.',
		'invitation-accepted': 'Your account is ready. Sign in with the password you chose.',
		'invitation-confirm':
			'Your account is ready. Before you sign in, confirm your email address with the link we are sending you. If no email arrives, ask a platform admin to confirm the address.',
		'invitation-sign-in': 'Sign in to accept the invitation.',
		'signed-out': 'You are signed out.'
	};
	const reason = $derived(data.reason ? REASONS[data.reason] : undefined);
	const loginAction = $derived(
		data.next ? `?/login&next=${encodeURIComponent(data.next)}` : '?/login'
	);
	// The email field, so that "Forgot password?" can carry it over. It starts with the address
	// of a refused attempt, which the server sends back.
	let email = $state(untrack(() => form?.email ?? data.email));
</script>

<svelte:head>
	<title>Sign in · pacod-gw</title>
</svelte:head>

<AuthCard title="Welcome back" subtitle="Sign in to the gateway console.">
	{#if form && 'unverified' in form && form.unverified}
		<div class="message error" role="alert">
			<p>Confirm your email address first, with the link in the email you were sent.</p>
			<form method="POST" action="?/resend" use:enhance>
				<input type="hidden" name="email" value={email} />
				<button type="submit" class="link-button">Send a new link</button>
			</form>
		</div>
	{:else if form && 'resent' in form && form.resent}
		<div class="message" role="status">
			<p>
				If {form.email} belongs to an account that is not confirmed yet, a new link is on its way.
			</p>
		</div>
	{:else if form && 'problem' in form && form.problem}
		<div class="message error" role="alert">
			<p>{form.problem.message}</p>
			{#if form.problem.type !== 'unauthorized' && form.problem.hint}
				<p class="hint">{form.problem.hint}</p>
			{/if}
		</div>
	{:else if reason}
		<div class="message" role="status"><p>{reason}</p></div>
	{/if}

	<form
		method="POST"
		action={loginAction}
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
				bind:value={email}
			/>
		</div>
		<div class="field">
			<label for="password">Password</label>
			<input
				id="password"
				name="password"
				type="password"
				autocomplete="current-password"
				placeholder="••••••••"
				required
			/>
		</div>
		<div class="options">
			<label>
				<input
					type="checkbox"
					name="remember_me"
					value="1"
					checked={form && 'remember' in form ? form.remember : false}
				/>
				Keep me logged in
			</label>
			<a href="/forgot-password{email ? `?email=${encodeURIComponent(email)}` : ''}">
				Forgot password?
			</a>
		</div>
		<button type="submit" class="btn btn-primary btn-wide" disabled={busy}>
			{busy ? 'Signing in…' : 'Sign in'}
		</button>
	</form>
</AuthCard>
