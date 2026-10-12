<script lang="ts">
	import { Button, Notice, PasswordField, TextField } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { del, get, patch, post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { formatTime } from '#lib/format.ts';
	import { m } from '#lib/paraglide/messages.js';
	import { me } from '#lib/me.svelte.ts';
	import Section from '#lib/components/Section.svelte';

	interface AccountRow {
		id: string;
		email: string;
		display_name: string;
		status: string;
		email_verified: boolean;
		signup_realm: string;
		created_at: string;
		last_login_at: string | null;
	}

	let q = $state('');
	let rows = $state<AccountRow[]>([]);
	let next = $state<string | null>(null);
	let msg = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	let busy = $state(false);
	let email = $state('');
	let name = $state('');
	let password = $state('');

	async function search(after = '') {
		const r = await get<{ accounts: AccountRow[]; next: string | null }>(
			`admin/accounts?q=${encodeURIComponent(q)}&after=${encodeURIComponent(after)}&limit=50`
		);
		rows = after ? [...rows, ...r.accounts] : r.accounts;
		next = r.next;
	}
	onMount(() => search());

	async function act(label: string, action: () => Promise<unknown>) {
		busy = true;
		try {
			await action();
			msg = { tone: 'success', text: label };
			await search();
		} catch (x) {
			msg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}

	const toggle = (a: AccountRow) =>
		act(
			a.status === 'active'
				? m.admin_disabled({ email: a.email })
				: m.admin_enabled({ email: a.email }),
			() =>
				patch(`admin/accounts/${a.id}`, { status: a.status === 'active' ? 'disabled' : 'active' })
		);
	const reset = (a: AccountRow) =>
		act(m.admin_reset_sent({ email: a.email }), () =>
			post(`admin/accounts/${a.id}/password-reset`)
		);
	const confirmEmail = (a: AccountRow) =>
		act(m.admin_email_confirmed({ email: a.email }), () =>
			post(`admin/accounts/${a.id}/email-confirmation`)
		);
	const remove = (a: AccountRow) => {
		if (confirm(m.admin_delete_confirm({ email: a.email })))
			act(m.admin_deleted({ email: a.email }), () => del(`admin/accounts/${a.id}`));
	};

	function create(e: SubmitEvent) {
		e.preventDefault();
		act(m.admin_created({ email }), async () => {
			await post('admin/accounts', { email, display_name: name, password: password || undefined });
			email = name = password = '';
		});
	}
</script>

<svelte:head><title>{m.nav_admin_accounts()}</title></svelte:head>

<h1>{m.nav_admin_accounts()}</h1>
{#if msg}<Notice tone={msg.tone}>{msg.text}</Notice>{/if}

<Section title={m.admin_accounts()}>
	<form class="row" onsubmit={(e) => (e.preventDefault(), search())}>
		<TextField label={m.admin_search()} type="search" bind:value={q} />
		<Button type="submit" variant="secondary">{m.admin_search_submit()}</Button>
	</form>
	<table class="list">
		<thead
			><tr
				><th>{m.field_email()}</th><th>{m.admin_status()}</th><th>{m.admin_last_sign_in()}</th><th
					><span class="visually-hidden">{m.ui_actions()}</span></th
				></tr
			></thead
		>
		<tbody>
			{#each rows as a (a.id)}
				<tr>
					<td
						>{a.email}{#if a.display_name}<br /><span class="muted">{a.display_name}</span>{/if}</td
					>
					<td>
						{a.status === 'active' ? m.admin_active() : m.admin_inactive()}
						{#if !a.email_verified}<br /><span class="muted small">{m.admin_unconfirmed()}</span
							>{/if}
					</td>
					<td>{formatTime(a.last_login_at)}</td>
					<td class="row">
						{#if a.id !== me.value?.account.id}
							<Button variant="quiet" disabled={busy} onclick={() => toggle(a)}
								>{a.status === 'active' ? m.admin_disable() : m.admin_enable()}</Button
							>
						{/if}
						<Button
							variant="quiet"
							disabled={busy || a.status !== 'active'}
							onclick={() => reset(a)}>{m.admin_send_reset()}</Button
						>
						{#if !a.email_verified}<Button
								variant="quiet"
								disabled={busy}
								onclick={() => confirmEmail(a)}>{m.admin_confirm_email()}</Button
							>{/if}
						{#if a.id !== me.value?.account.id}<Button
								variant="quiet"
								disabled={busy}
								onclick={() => remove(a)}>{m.admin_delete()}</Button
							>{/if}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
	{#if next}<p>
			<Button variant="secondary" onclick={() => search(next ?? '')}>{m.ui_more()}</Button>
		</p>{/if}
</Section>

<Section title={m.admin_create()} intro={m.admin_create_intro()}>
	<form class="stack" onsubmit={create}>
		<TextField
			label={m.field_email()}
			type="email"
			required
			bind:value={email}
			autocomplete="off"
		/>
		<TextField label={m.field_name()} bind:value={name} autocomplete="off" />
		<PasswordField
			label={m.admin_initial_password()}
			autocomplete="new-password"
			bind:value={password}
			showLabel={m.field_show()}
			hideLabel={m.field_hide()}
		/>
		<div><Button type="submit" variant="primary" {busy}>{m.admin_create_submit()}</Button></div>
	</form>
</Section>
