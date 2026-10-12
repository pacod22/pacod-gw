<script lang="ts">
	import {
		AuthCard,
		Button,
		Checkbox,
		Divider,
		Notice,
		PasswordField,
		ProviderButton,
		TextField
	} from '@pacod/ui';
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { get, post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import type { Flow, Next } from '#lib/flow.ts';
	import { brand, go } from '#lib/page.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { loadRealm } from '#lib/realm.svelte.ts';
	import PageFooter from '#lib/components/PageFooter.svelte';

	const flowId = page.url.searchParams.get('flow');
	let flow = $state<Flow | null>(null);
	let view = $state<'loading' | 'form' | 'closed' | 'sent' | 'expired'>('loading');
	let name = $state('');
	let email = $state('');
	let password = $state('');
	let terms = $state(false);
	let sentTo = $state('');
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let busy = $state(false);

	onMount(async () => {
		loadRealm().catch(() => {});
		if (!flowId) {
			// From the account pages: a sign-up of pacod-id itself.
			try {
				go(
					(await post<{ to: string }>('account/sign-in', { create: true, return_to: '/account' }))
						.to
				);
			} catch {
				view = 'expired';
			}
			return;
		}
		try {
			flow = await get<Flow>(`flows/${flowId}`);
		} catch {
			view = 'expired';
			return;
		}
		email = flow.login_hint ?? '';
		view = flow.self_signup ? 'form' : 'closed';
	});

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = null;
		try {
			const r = await post<Next & { email?: string }>(`flows/${flowId}/signup`, {
				email,
				password,
				display_name: name,
				accept_terms: terms,
				locale: document.documentElement.lang
			});
			if (r.next === 'redirect') return go(r.to);
			sentTo = r.email ?? email;
			view = 'sent';
		} catch (x) {
			error = explain(x);
		}
		busy = false;
	}

	async function resume() {
		busy = true;
		try {
			const r = await post<Next>(`flows/${flowId}/resume`);
			if (r.next === 'redirect') return go(r.to);
			notice = m.verify_still_waiting();
		} catch (x) {
			error = explain(x);
		}
		busy = false;
	}

	async function google() {
		busy = true;
		try {
			go((await post<{ to: string }>(`flows/${flowId}/google`)).to);
		} catch (x) {
			error = explain(x);
			busy = false;
		}
	}
</script>

<svelte:head><title>{m.signup_title()}</title></svelte:head>

<AuthCard title={view === 'sent' ? m.verify_wait_title() : m.signup_title()} brand={brand()}>
	<div class="stack">
		{#if notice}<Notice tone="info">{notice}</Notice>{/if}
		{#if error}<Notice tone="error">{error}</Notice>{/if}
		{#if view === 'loading'}
			<p class="muted">{m.ui_loading()}</p>
		{:else if view === 'form'}
			<form class="stack" onsubmit={submit}>
				<TextField label={m.field_name()} name="name" autocomplete="name" bind:value={name} />
				<TextField
					label={m.field_email()}
					type="email"
					name="email"
					autocomplete="email"
					required
					bind:value={email}
				/>
				<PasswordField
					label={m.field_password()}
					autocomplete="new-password"
					required
					minLength={flow?.password_min_length}
					missing={(n) => m.field_missing({ n })}
					bind:value={password}
					showLabel={m.field_show()}
					hideLabel={m.field_hide()}
				/>
				{#if flow?.terms_url}
					<Checkbox label={m.signup_terms()} bind:checked={terms} required />
					<a class="small" href={flow.terms_url} rel="external noopener">{m.ui_terms()}</a>
				{/if}
				<Button type="submit" variant="primary" wide {busy}>{m.signup_submit()}</Button>
			</form>
			{#if flow?.google}
				<Divider label={m.ui_or()} />
				<ProviderButton
					provider="google"
					label={m.signup_google()}
					onclick={google}
					disabled={busy}
				/>
			{/if}
			<div class="links">
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- a resolved route with the flow's query -->
				<a href={`${resolve('/login')}?flow=${flowId}`}>{m.signup_have_account()}</a>
			</div>
		{:else if view === 'sent'}
			<p>{m.verify_wait_intro({ email: sentTo })}</p>
			<Button variant="primary" wide {busy} onclick={resume}>{m.verify_wait_done()}</Button>
		{:else if view === 'closed'}
			<p>{m.signup_closed()}</p>
			<div class="links">
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- a resolved route with the flow's query -->
				<a href={`${resolve('/login')}?flow=${flowId}`}>{m.signup_have_account()}</a>
			</div>
		{:else}
			<p>{m.flow_expired_intro()}</p>
		{/if}
	</div>
	{#snippet footer()}<PageFooter />{/snippet}
</AuthCard>
