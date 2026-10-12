<script lang="ts">
	import { Button, Notice, TextField } from '@pacod/ui';
	import { patch, post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { chooseLanguage, LANGUAGE_NAMES } from '#lib/i18n.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { me } from '#lib/me.svelte.ts';
	import { realm } from '#lib/realm.svelte.ts';
	import Reauth from '#lib/components/Reauth.svelte';
	import Section from '#lib/components/Section.svelte';

	const account = $derived(me.value!.account);
	let name = $state(me.value?.account.display_name ?? '');
	let locale = $state(me.value?.account.locale || document.documentElement.lang);
	let newEmail = $state('');
	let profileMsg = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	let emailMsg = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	let needReauth = $state(false);
	let busy = $state(false);

	async function saveProfile(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		try {
			await patch('account', { display_name: name, locale });
			profileMsg = { tone: 'success', text: m.account_saved() };
			if (locale !== document.documentElement.lang) chooseLanguage(locale);
		} catch (x) {
			profileMsg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}

	async function changeEmail(e?: SubmitEvent) {
		e?.preventDefault();
		busy = true;
		try {
			await post('account/email', { email: newEmail });
			emailMsg = { tone: 'success', text: m.account_email_sent({ email: newEmail }) };
			needReauth = false;
		} catch (x) {
			if ((x as { code?: string }).code === 'reauth_required') needReauth = true;
			else emailMsg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}
</script>

<svelte:head><title>{m.nav_profile()}</title></svelte:head>

<h1>{m.nav_profile()}</h1>

<Section title={m.account_profile()}>
	{#if profileMsg}<Notice tone={profileMsg.tone}>{profileMsg.text}</Notice>{/if}
	<form class="stack" onsubmit={saveProfile}>
		<TextField label={m.field_name()} autocomplete="name" bind:value={name} />
		<div class="pid-field">
			<label for="locale">{m.ui_language()}</label>
			<select id="locale" bind:value={locale}>
				{#each realm.value?.locales ?? [locale] as code (code)}
					<option value={code}>{LANGUAGE_NAMES[code] ?? code}</option>
				{/each}
			</select>
		</div>
		<div><Button type="submit" variant="primary" {busy}>{m.ui_save()}</Button></div>
	</form>
</Section>

<Section
	title={m.account_email()}
	intro={account.email_verified
		? m.account_email_verified({ email: account.email })
		: m.account_email_unverified({ email: account.email })}
>
	{#if emailMsg}<Notice tone={emailMsg.tone}>{emailMsg.text}</Notice>{/if}
	{#if needReauth}
		<Reauth done={() => changeEmail()} />
	{:else}
		<form class="stack" onsubmit={changeEmail}>
			<TextField
				label={m.account_new_email()}
				type="email"
				autocomplete="email"
				required
				bind:value={newEmail}
			/>
			<div>
				<Button type="submit" variant="secondary" {busy}>{m.account_change_email()}</Button>
			</div>
		</form>
	{/if}
</Section>

<style>
	select {
		height: var(--pid-control-height);
		border: 1px solid var(--pid-border-strong);
		border-radius: var(--pid-radius);
		background: var(--pid-surface);
		color: var(--pid-text);
		font: inherit;
		padding: 0 var(--pid-space-3);
		max-width: 16rem;
	}
</style>
