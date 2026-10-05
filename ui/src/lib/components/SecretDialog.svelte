<script lang="ts">
	import { untrack } from 'svelte';
	import type { ActionOutcome, OnceSecret } from '#lib/server/resource.ts';
	import Dialog from './Dialog.svelte';

	/**
	 * Shows a secret that the API returns a single time. It opens when an action answers with
	 * one and forgets it when it is closed.
	 */
	interface Props {
		/** The page's `form` prop. */
		outcome: ActionOutcome | null | undefined;
	}

	let { outcome }: Props = $props();

	let open = $state(false);
	let secret: OnceSecret | null = $state(null);
	let copied = $state(false);
	let field: HTMLInputElement | undefined = $state();

	$effect(() => {
		const next = outcome?.ok ? outcome.secret : undefined;
		if (next) {
			untrack(() => {
				secret = next;
				copied = false;
				open = true;
			});
		}
	});

	async function copy() {
		if (!secret) return;
		try {
			await navigator.clipboard.writeText(secret.value);
			copied = true;
		} catch {
			// No clipboard access: select the text so that it can be copied by hand.
			field?.select();
		}
	}

	function closed() {
		secret = null;
		copied = false;
	}
</script>

<Dialog bind:open title={secret?.title ?? 'Secret'} onclose={closed}>
	{#if secret}
		<div class="stack">
			<p>{secret.note}</p>
			<div class="secret">
				<label for="once-secret" class="visually-hidden">Secret</label>
				<input
					id="once-secret"
					bind:this={field}
					class="mono"
					type="text"
					readonly
					value={secret.value}
					onfocus={(event) => event.currentTarget.select()}
				/>
				<button type="button" class="btn btn-primary" onclick={copy}>
					{copied ? 'Copied' : 'Copy'}
				</button>
			</div>
			<p class="muted small" role="status">
				{copied
					? 'The secret is on the clipboard.'
					: 'After you close this dialog it cannot be shown again.'}
			</p>
			<div class="buttons">
				<button type="button" class="btn" onclick={() => (open = false)}>I have stored it</button>
			</div>
		</div>
	{/if}
</Dialog>

<style>
	.secret {
		display: flex;
		gap: var(--space-2);
	}

	.secret input {
		flex: 1;
		min-width: 0;
	}

	.buttons {
		display: flex;
		justify-content: flex-end;
	}
</style>
