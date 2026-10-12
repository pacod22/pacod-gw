<script lang="ts">
	import { untrack } from 'svelte';
	import { enhance } from '$app/forms';
	import { roleLabel } from '#lib/access.ts';
	import { AuthCard } from '@pacod/ui';
	import { ROLE_HELP } from '#lib/members.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	let busy = $state(false);
	// Kept when the form comes back with a problem; typed again otherwise.
	let displayName = $state(
		untrack(() => (form && 'displayName' in form ? (form.displayName ?? '') : ''))
	);

	const inv = $derived(data.invitation);
	const invalid = $derived(!inv || (form && 'invalid' in form && form.invalid));
	const place = $derived(
		inv
			? inv.workspace_name
				? `the workspace ${inv.workspace_name} of ${inv.organisation_name}`
				: `the organisation ${inv.organisation_name}`
			: ''
	);
	const problem = $derived(form && 'problem' in form ? form.problem : null);
	const here = $derived(`/invite?token=${encodeURIComponent(data.token)}`);
	const submitting = () => {
		busy = true;
		return async ({ update }: { update: () => Promise<void> }) => {
			await update();
			busy = false;
		};
	};
</script>

<svelte:head>
	<title>Invitation · pacod-gw</title>
</svelte:head>

<AuthCard
	title={invalid ? 'This invitation cannot be used' : 'You are invited'}
	subtitle={invalid || !inv ? undefined : `${inv.invited_by} invited you to ${place}.`}
>
	{#if invalid || !inv}
		<div class="message error" role="alert">
			<p>
				The link is invalid, was already used or has expired, or the invitation was withdrawn. Ask
				whoever invited you for a new one.
			</p>
		</div>
		<a class="btn btn-primary btn-wide" href="/login">Go to sign in</a>
	{:else}
		<div class="offer">
			<p><span class="muted">Role</span> <strong>{roleLabel(inv.role)}</strong></p>
			<p class="muted small">{ROLE_HELP[inv.role]}</p>
			<p><span class="muted">For</span> <strong>{inv.email}</strong></p>
		</div>

		{#if problem}
			<div class="message error" role="alert"><p>{problem.message}</p></div>
		{/if}

		{#if !inv.account_exists}
			{#if data.me}
				<div class="message" role="status">
					<p>
						You are signed in as {data.me}. The invitation is for {inv.email}, which has no account
						yet: sign out to create it.
					</p>
				</div>
				<form method="POST" action="/logout">
					<input type="hidden" name="next" value={here} />
					<button type="submit" class="btn btn-wide">Sign out and continue</button>
				</form>
			{:else}
				<form method="POST" action="?/create" use:enhance={submitting}>
					<input type="hidden" name="token" value={data.token} />
					<div class="field">
						<label for="display_name">Your name</label>
						<input
							id="display_name"
							name="display_name"
							autocomplete="name"
							placeholder={inv.email}
							bind:value={displayName}
						/>
					</div>
					<div class="field">
						<label for="password">Choose a password</label>
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
						<label for="repeat">The password again</label>
						<input
							id="repeat"
							name="repeat"
							type="password"
							autocomplete="new-password"
							minlength="12"
							required
						/>
					</div>
					<p class="muted small">
						At least 12 characters. You sign in with your email address and it.
					</p>
					<button type="submit" class="btn btn-primary btn-wide" disabled={busy}>
						{busy ? 'Creating your account…' : 'Create account and accept'}
					</button>
				</form>
			{/if}
		{:else if data.me === inv.email}
			<form method="POST" action="?/accept" use:enhance={submitting}>
				<input type="hidden" name="token" value={data.token} />
				<button type="submit" class="btn btn-primary btn-wide" disabled={busy}>
					{busy ? 'Accepting…' : 'Accept the invitation'}
				</button>
			</form>
		{:else if data.me}
			<div class="message" role="status">
				<p>
					You are signed in as {data.me}, but the invitation is for {inv.email}. Sign in to that
					account to accept it.
				</p>
			</div>
			<form method="POST" action="/logout">
				<!-- Back here once signed out, which then offers to sign in as the invited account. -->
				<input type="hidden" name="next" value={here} />
				<button type="submit" class="btn btn-wide">Sign out and sign in as {inv.email}</button>
			</form>
		{:else}
			<p class="muted small">You already have an account. Sign in to accept the invitation.</p>
			<a
				class="btn btn-primary btn-wide"
				href="/login?{new URLSearchParams({
					email: inv.email,
					reason: 'invitation-sign-in',
					next: here
				})}"
			>
				Sign in to accept
			</a>
		{/if}
	{/if}

	{#snippet footer()}
		{#if data.me}<a href="/">Back to the console</a>{:else}<a href="/login">Sign in</a>{/if}
	{/snippet}
</AuthCard>

<style>
	.offer {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		padding: var(--space-3) var(--space-4);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		margin-bottom: var(--space-3);
		background: var(--surface-2);
	}
</style>
