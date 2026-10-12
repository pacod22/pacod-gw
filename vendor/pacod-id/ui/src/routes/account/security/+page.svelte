<script lang="ts">
	import { Button, Notice, PasswordField, TextField } from '@pacod/ui';
	import { del, post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { me } from '#lib/me.svelte.ts';
	import { realm } from '#lib/realm.svelte.ts';
	import Reauth from '#lib/components/Reauth.svelte';
	import Section from '#lib/components/Section.svelte';

	const account = $derived(me.value!.account);
	let current = $state('');
	let next = $state('');
	let pwMsg = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	let needReauth = $state<'password' | 'delete' | null>(null);
	let confirmEmail = $state('');
	let delMsg = $state<string | null>(null);
	let busy = $state(false);

	async function setPassword(e?: SubmitEvent) {
		e?.preventDefault();
		busy = true;
		try {
			const r = await post<{ other_sessions_ended: number }>('account/password', {
				current: account.has_password ? current : undefined,
				password: next
			});
			pwMsg = { tone: 'success', text: m.security_password_changed({ n: r.other_sessions_ended }) };
			current = next = '';
			needReauth = null;
			me.value!.account.has_password = true;
		} catch (x) {
			if ((x as { code?: string }).code === 'reauth_required') needReauth = 'password';
			else pwMsg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}

	async function remove() {
		busy = true;
		try {
			await del('account');
			window.location.assign('/account');
		} catch (x) {
			if ((x as { code?: string }).code === 'reauth_required') needReauth = 'delete';
			else delMsg = explain(x);
			busy = false;
		}
	}
</script>

<svelte:head><title>{m.nav_security()}</title></svelte:head>

<h1>{m.nav_security()}</h1>

<Section
	title={account.has_password ? m.security_change_password() : m.security_set_password()}
	intro={account.has_password ? undefined : m.security_set_password_intro()}
>
	{#if pwMsg}<Notice tone={pwMsg.tone}>{pwMsg.text}</Notice>{/if}
	{#if needReauth === 'password'}
		<Reauth done={() => setPassword()} />
	{:else}
		<form class="stack" onsubmit={setPassword}>
			{#if account.has_password}
				<PasswordField
					label={m.field_current_password()}
					autocomplete="current-password"
					required
					bind:value={current}
					showLabel={m.field_show()}
					hideLabel={m.field_hide()}
				/>
			{/if}
			<PasswordField
				label={m.field_new_password()}
				autocomplete="new-password"
				required
				minLength={realm.value?.password_min_length}
				missing={(n) => m.field_missing({ n })}
				bind:value={next}
				showLabel={m.field_show()}
				hideLabel={m.field_hide()}
			/>
			<p class="small muted">{m.security_password_note()}</p>
			<div><Button type="submit" variant="primary" {busy}>{m.ui_save()}</Button></div>
		</form>
	{/if}
</Section>

<Section title={m.security_delete()} intro={m.security_delete_intro()}>
	{#if delMsg}<Notice tone="error">{delMsg}</Notice>{/if}
	{#if needReauth === 'delete'}
		<Reauth done={() => remove()} />
	{:else}
		<div class="stack">
			<TextField
				label={m.security_delete_confirm({ email: account.email })}
				bind:value={confirmEmail}
				autocomplete="off"
			/>
			<div>
				<Button
					variant="danger"
					disabled={confirmEmail.trim().toLowerCase() !== account.email}
					{busy}
					onclick={remove}>{m.security_delete_submit()}</Button
				>
			</div>
		</div>
	{/if}
</Section>
