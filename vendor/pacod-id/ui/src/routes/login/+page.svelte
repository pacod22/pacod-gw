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
	import { explain, googleError } from '#lib/errors.ts';
	import type { Flow, Next } from '#lib/flow.ts';
	import { brand, go } from '#lib/page.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { loadRealm } from '#lib/realm.svelte.ts';
	import PageFooter from '#lib/components/PageFooter.svelte';

	type View = 'loading' | 'password' | 'link' | 'verify' | 'change' | 'expired' | 'none';

	const flowId = page.url.searchParams.get('flow');
	let flow = $state<Flow | null>(null);
	let view = $state<View>('loading');
	let email = $state('');
	let password = $state('');
	let remember = $state(false);
	let newPassword = $state('');
	let minLength = $state(12);
	let waitingFor = $state('');
	let error = $state<string | null>(null);
	let notice = $state<string | null>(googleError(page.url.searchParams.get('error')));
	let busy = $state(false);

	const title = $derived(
		{
			loading: m.login_title(),
			password: m.login_title(),
			link: m.link_google_title(),
			verify: m.verify_wait_title(),
			change: m.change_password_title(),
			expired: m.flow_expired_title(),
			none: m.login_title()
		}[view]
	);

	onMount(async () => {
		loadRealm().catch(() => {});
		if (!flowId) {
			view = 'none';
			return;
		}
		try {
			flow = await get<Flow>(`flows/${flowId}`);
		} catch {
			view = 'expired';
			return;
		}
		email = flow.link_email ?? flow.login_hint ?? '';
		minLength = flow.password_min_length;
		view =
			page.url.searchParams.get('next') === 'verify_email' || flow.stage === 'verify_email'
				? 'verify'
				: flow.stage === 'link_google'
					? 'link'
					: flow.stage === 'change_password'
						? 'change'
						: 'password';
		waitingFor = email;
	});

	function follow(r: Next) {
		if (r.next === 'redirect') {
			go(r.to);
		} else if (r.next === 'verify_email') {
			waitingFor = r.email;
			view = 'verify';
			busy = false;
		} else if (r.next === 'change_password') {
			minLength = r.min_length;
			view = 'change';
			busy = false;
		}
	}

	async function run(action: () => Promise<void>) {
		busy = true;
		error = null;
		try {
			await action();
		} catch (e) {
			error = explain(e);
			busy = false;
		}
	}

	const signIn = (e: SubmitEvent) => {
		e.preventDefault();
		return run(async () =>
			follow(
				await post<Next>(`flows/${flowId}/password`, { email, password, remember_me: remember })
			)
		);
	};
	const google = () =>
		run(async () => go((await post<{ to: string }>(`flows/${flowId}/google`)).to));
	const resume = () =>
		run(async () => {
			const r = await post<Next>(`flows/${flowId}/resume`);
			if (r.next === 'verify_email') {
				notice = m.verify_still_waiting();
				busy = false;
			} else follow(r);
		});
	const resend = () =>
		run(async () => {
			await post('verification', { email: waitingFor });
			notice = m.verify_resent();
			busy = false;
		});
	const change = (e: SubmitEvent) => {
		e.preventDefault();
		return run(async () =>
			follow(await post<Next>(`flows/${flowId}/password-change`, { password: newPassword }))
		);
	};
</script>

<svelte:head><title>{title}</title></svelte:head>

<AuthCard {title} brand={brand()} subtitle={view === 'password' ? m.login_subtitle() : undefined}>
	<div class="stack">
		{#if notice}<Notice tone="info">{notice}</Notice>{/if}
		{#if error}<Notice tone="error">{error}</Notice>{/if}

		{#if view === 'loading'}
			<p class="muted" aria-busy="true">{m.ui_loading()}</p>
		{:else if view === 'password' || view === 'link'}
			{#if view === 'link'}<p>{m.link_google_intro({ email })}</p>{/if}
			<form class="stack" onsubmit={signIn}>
				<TextField
					label={m.field_email()}
					type="email"
					name="email"
					autocomplete="username"
					required
					bind:value={email}
					readonly={view === 'link'}
				/>
				<PasswordField
					label={m.field_password()}
					name="password"
					autocomplete="current-password"
					required
					bind:value={password}
					showLabel={m.field_show()}
					hideLabel={m.field_hide()}
				/>
				{#if view === 'password'}<Checkbox
						label={m.login_remember()}
						bind:checked={remember}
					/>{/if}
				<Button type="submit" variant="primary" wide {busy}
					>{view === 'link' ? m.link_google_submit() : m.login_submit()}</Button
				>
			</form>
			{#if view === 'password' && flow?.google}
				<Divider label={m.ui_or()} />
				<ProviderButton
					provider="google"
					label={m.login_google()}
					onclick={google}
					disabled={busy}
				/>
			{/if}
			{#if view === 'password'}
				<div class="links">
					<a href={resolve('/forgot-password')}>{m.login_forgot()}</a>
					<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- a resolved route with the flow's query -->
					{#if flow?.self_signup}<a href={`${resolve('/signup')}?flow=${flowId}`}
							>{m.login_create()}</a
						>{/if}
				</div>
			{/if}
		{:else if view === 'verify'}
			<p>{m.verify_wait_intro({ email: waitingFor })}</p>
			<div class="stack">
				<Button variant="primary" wide {busy} onclick={resume}>{m.verify_wait_done()}</Button>
				<Button wide variant="secondary" disabled={busy} onclick={resend}
					>{m.verify_resend()}</Button
				>
			</div>
		{:else if view === 'change'}
			<p>{m.change_password_intro({ min: minLength })}</p>
			<form class="stack" onsubmit={change}>
				<PasswordField
					label={m.field_new_password()}
					autocomplete="new-password"
					required
					{minLength}
					missing={(n) => m.field_missing({ n })}
					bind:value={newPassword}
					showLabel={m.field_show()}
					hideLabel={m.field_hide()}
				/>
				<Button type="submit" variant="primary" wide {busy}>{m.change_password_submit()}</Button>
			</form>
		{:else if view === 'expired'}
			<p>{m.flow_expired_intro()}</p>
		{:else}
			<p>{m.login_no_flow()}</p>
			<Button variant="primary" wide href={resolve('/account')}>{m.login_to_account()}</Button>
		{/if}
	</div>
	{#snippet footer()}<PageFooter />{/snippet}
</AuthCard>
