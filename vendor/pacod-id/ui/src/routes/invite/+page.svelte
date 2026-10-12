<script lang="ts">
	import { AuthCard, Button, Notice, PasswordField, TextField } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { brand, go, takeToken } from '#lib/page.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { loadRealm, realm } from '#lib/realm.svelte.ts';
	import PageFooter from '#lib/components/PageFooter.svelte';

	interface Invitation {
		email: string;
		role_label: string;
		scope_label: string;
		inviter_name: string;
		account_exists: boolean;
		signed_in_as: string | null;
		expires_at: string;
	}

	// Kept for this tab only, so that signing in on the way does not put the token back into an
	// address.
	const KEY = 'pid_invite';
	let token: string | null = null;
	let inv = $state<Invitation | null>(null);
	let view = $state<'loading' | 'offer' | 'accepted' | 'created' | 'failed'>('loading');
	let name = $state('');
	let password = $state('');
	let confirm = $state(false);
	let to = $state<string | null>(null);
	let error = $state<string | null>(null);
	let busy = $state(false);

	onMount(async () => {
		loadRealm().catch(() => {});
		token = takeToken() ?? sessionStorage.getItem(KEY);
		if (!token) {
			view = 'failed';
			error = m.error_link_invalid();
			return;
		}
		sessionStorage.setItem(KEY, token);
		try {
			inv = await post<Invitation>('invitations/lookup', { token });
			view = 'offer';
		} catch (x) {
			sessionStorage.removeItem(KEY);
			error = explain(x);
			view = 'failed';
		}
	});

	async function signIn() {
		busy = true;
		try {
			go(
				(
					await post<{ to: string }>('account/sign-in', {
						return_to: '/invite',
						login_hint: inv?.email
					})
				).to
			);
		} catch (x) {
			error = explain(x);
			busy = false;
		}
	}

	async function accept(e?: SubmitEvent) {
		e?.preventDefault();
		busy = true;
		error = null;
		try {
			const body = inv?.account_exists ? { token } : { token, password, display_name: name };
			const r = await post<{ next: string; to: string | null; needs_confirmation?: boolean }>(
				'invitations/accept',
				body
			);
			sessionStorage.removeItem(KEY);
			to = r.to;
			confirm = !!r.needs_confirmation;
			view = r.next === 'continue' ? 'accepted' : 'created';
		} catch (x) {
			error = explain(x);
		}
		busy = false;
	}
</script>

<svelte:head><title>{m.invite_title()}</title></svelte:head>

<AuthCard title={m.invite_title()} brand={brand()}>
	<div class="stack">
		{#if error}<Notice tone="error">{error}</Notice>{/if}
		{#if view === 'loading'}
			<p class="muted">{m.ui_loading()}</p>
		{:else if view === 'offer' && inv}
			<p>
				{m.invite_offer({
					inviter: inv.inviter_name || '—',
					scope: inv.scope_label,
					role: inv.role_label
				})}
			</p>
			<p class="muted small">{m.invite_for({ email: inv.email })}</p>
			{#if inv.account_exists && inv.signed_in_as === inv.email}
				<Button wide {busy} onclick={() => accept()}>{m.invite_accept()}</Button>
			{:else if inv.account_exists}
				{#if inv.signed_in_as}<Notice tone="warning"
						>{m.invite_other_account({ email: inv.signed_in_as })}</Notice
					>{/if}
				<Button wide {busy} onclick={signIn}>{m.invite_sign_in({ email: inv.email })}</Button>
			{:else}
				<form class="stack" onsubmit={accept}>
					<TextField label={m.field_name()} autocomplete="name" bind:value={name} />
					<PasswordField
						label={m.field_choose_password()}
						autocomplete="new-password"
						required
						minLength={realm.value?.role_holder_password_min_length ?? 12}
						missing={(n) => m.field_missing({ n })}
						bind:value={password}
						showLabel={m.field_show()}
						hideLabel={m.field_hide()}
					/>
					<Button type="submit" variant="primary" wide {busy}>{m.invite_create()}</Button>
				</form>
			{/if}
		{:else if view === 'accepted'}
			<Notice tone="success">{m.invite_accepted()}</Notice>
			<Button variant="primary" wide href={to ?? resolve('/account/organisations')}
				>{m.ui_continue()}</Button
			>
		{:else if view === 'created'}
			<Notice tone="success">{m.invite_created()}</Notice>
			{#if confirm}<p>{m.invite_confirm_address()}</p>{/if}
			<Button variant="primary" wide href={to ?? resolve('/account')}>{m.ui_continue()}</Button>
		{/if}
	</div>
	{#snippet footer()}<PageFooter />{/snippet}
</AuthCard>
