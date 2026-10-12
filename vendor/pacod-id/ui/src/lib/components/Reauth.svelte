<script lang="ts">
	import { Button, Notice, PasswordField } from '@pacod/ui';
	import { post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { m } from '#lib/paraglide/messages.js';

	/** Asks for the password again; `done` runs once the service counts the sign-in as recent. */
	let { done }: { done: () => void } = $props();
	let password = $state('');
	let error = $state<string | null>(null);
	let busy = $state(false);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = null;
		try {
			await post('account/reauth', { password });
			password = '';
			done();
		} catch (x) {
			error = explain(x);
		}
		busy = false;
	}
</script>

<form class="stack" onsubmit={submit}>
	<p class="small muted">{m.reauth_intro()}</p>
	{#if error}<Notice tone="error">{error}</Notice>{/if}
	<PasswordField
		label={m.field_password()}
		autocomplete="current-password"
		required
		bind:value={password}
		showLabel={m.field_show()}
		hideLabel={m.field_hide()}
	/>
	<Button type="submit" variant="secondary" {busy}>{m.reauth_submit()}</Button>
</form>
