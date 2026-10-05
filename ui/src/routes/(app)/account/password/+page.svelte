<script lang="ts">
	import { enhance } from '$app/forms';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import type { PageProps } from './$types';

	let { form }: PageProps = $props();
	let busy = $state(false);

	const errors = $derived(form?.problem?.fields ?? {});
</script>

<div class="stack-lg narrow">
	<PageHeader
		title="Change password"
		description="After the change every session of your account ends, this one included, and you sign in again with the new password."
	/>

	{#if form?.problem}
		<Notice tone="error" title={form.problem.title}>
			<p>{form.problem.message}</p>
		</Notice>
	{/if}

	<form
		method="POST"
		class="card stack"
		use:enhance={() => {
			busy = true;
			return async ({ update }) => {
				await update();
				busy = false;
			};
		}}
	>
		<div class="field">
			<label for="current">Current password</label>
			<input
				id="current"
				name="current"
				type="password"
				autocomplete="current-password"
				required
				aria-invalid={errors.current ? 'true' : undefined}
				aria-describedby={errors.current ? 'current-error' : undefined}
			/>
			{#if errors.current}<p class="error" id="current-error">{errors.current}</p>{/if}
		</div>
		<div class="field">
			<label for="new">New password</label>
			<input
				id="new"
				name="new"
				type="password"
				autocomplete="new-password"
				minlength="12"
				required
				aria-invalid={errors.new ? 'true' : undefined}
				aria-describedby="new-help{errors.new ? ' new-error' : ''}"
			/>
			<p class="muted small" id="new-help">At least 12 characters.</p>
			{#if errors.new}<p class="error" id="new-error">{errors.new}</p>{/if}
		</div>
		<div class="field">
			<label for="repeat">New password, again</label>
			<input
				id="repeat"
				name="repeat"
				type="password"
				autocomplete="new-password"
				required
				aria-invalid={errors.repeat ? 'true' : undefined}
				aria-describedby={errors.repeat ? 'repeat-error' : undefined}
			/>
			{#if errors.repeat}<p class="error" id="repeat-error">{errors.repeat}</p>{/if}
		</div>
		<div class="row">
			<button type="submit" class="btn btn-primary" disabled={busy}>
				{busy ? 'Changing…' : 'Change password'}
			</button>
			<a class="btn" href="/">Cancel</a>
		</div>
	</form>
</div>

<style>
	.narrow {
		max-width: 30rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	.error {
		color: var(--danger);
		font-size: var(--text-sm);
		font-weight: 600;
	}
</style>
