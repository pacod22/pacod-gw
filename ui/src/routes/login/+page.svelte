<script lang="ts">
	import { enhance } from '$app/forms';
	import Notice from '#lib/components/Notice.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	let busy = $state(false);

	const REASONS: Record<string, string> = {
		'password-changed': 'Your password was changed. Sign in with the new password.',
		'signed-out': 'You are signed out.'
	};
	const reason = $derived(data.reason ? REASONS[data.reason] : undefined);
</script>

<svelte:head>
	<title>Sign in · pacod-gw</title>
</svelte:head>

<main>
	<div class="card stack">
		<div>
			<p class="muted small">pacod-gw console</p>
			<h1>Sign in</h1>
		</div>

		{#if form?.problem}
			<Notice
				tone={form.problem.tone === 'info' ? 'warning' : form.problem.tone}
				title="Sign-in failed"
			>
				<p>{form.problem.message}</p>
				{#if form.problem.type !== 'unauthorized' && form.problem.hint}
					<p class="muted">{form.problem.hint}</p>
				{/if}
			</Notice>
		{:else if reason}
			<Notice tone="info"><p>{reason}</p></Notice>
		{/if}

		<form
			method="POST"
			class="stack"
			use:enhance={() => {
				busy = true;
				return async ({ update }) => {
					await update();
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
					required
					value={form?.email ?? ''}
				/>
			</div>
			<div class="field">
				<label for="password">Password</label>
				<input
					id="password"
					name="password"
					type="password"
					autocomplete="current-password"
					required
				/>
			</div>
			<button type="submit" class="btn btn-primary" disabled={busy}>
				{busy ? 'Signing in…' : 'Sign in'}
			</button>
		</form>
	</div>
</main>

<style>
	main {
		display: grid;
		place-items: center;
		min-height: 100dvh;
		padding: var(--space-4);
	}

	.card {
		width: min(24rem, 100%);
		padding: var(--space-5);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
</style>
