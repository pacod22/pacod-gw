<script lang="ts">
	import { Button, Notice, TextField } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { del, get, patch, post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { formatTime } from '#lib/format.ts';
	import { m } from '#lib/paraglide/messages.js';
	import Section from '#lib/components/Section.svelte';

	interface ServiceAccount {
		id: string;
		name: string;
		status: string;
		created_at: string;
	}
	interface Token {
		id: string;
		hint: string;
		status: string;
		expires_at: string | null;
		last_used_at: string | null;
		legacy_product: string | null;
	}

	let accounts = $state<ServiceAccount[]>([]);
	let tokens = $state<Record<string, Token[]>>({});
	let fresh = $state<{ account: string; token: string } | null>(null);
	let name = $state('');
	let msg = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	let busy = $state(false);

	async function load() {
		accounts = (await get<{ service_accounts: ServiceAccount[] }>('admin/service-accounts'))
			.service_accounts;
		const all = await Promise.all(
			accounts.map((a) => get<{ tokens: Token[] }>(`admin/service-accounts/${a.id}/tokens`))
		);
		tokens = Object.fromEntries(accounts.map((a, i) => [a.id, all[i].tokens]));
	}
	onMount(load);

	async function act(action: () => Promise<unknown>, done?: string) {
		busy = true;
		try {
			await action();
			msg = done ? { tone: 'success', text: done } : null;
			await load();
		} catch (x) {
			msg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}

	const create = (e: SubmitEvent) => {
		e.preventDefault();
		return act(async () => {
			await post('admin/service-accounts', { name });
			name = '';
		});
	};
	const newToken = (a: ServiceAccount) =>
		act(async () => {
			const r = await post<{ token: string }>(`admin/service-accounts/${a.id}/tokens`);
			fresh = { account: a.id, token: r.token };
		});
	const revoke = (t: Token) => act(() => del(`admin/service-tokens/${t.id}`), m.service_revoked());
	const toggle = (a: ServiceAccount) =>
		act(() =>
			patch(`admin/service-accounts/${a.id}`, {
				status: a.status === 'active' ? 'disabled' : 'active'
			})
		);
	const remove = (a: ServiceAccount) => {
		if (confirm(m.service_delete_confirm({ name: a.name })))
			act(() => del(`admin/service-accounts/${a.id}`), m.service_deleted());
	};
</script>

<svelte:head><title>{m.nav_admin_service_accounts()}</title></svelte:head>

<h1>{m.nav_admin_service_accounts()}</h1>
{#if msg}<Notice tone={msg.tone}>{msg.text}</Notice>{/if}
{#if fresh}
	<Notice tone="warning">{m.service_token_once()}</Notice>
	<p><code class="token">{fresh.token}</code></p>
	<p>
		<Button variant="secondary" onclick={() => (fresh = null)}>{m.service_token_copied()}</Button>
	</p>
{/if}

{#each accounts as a (a.id)}
	<Section
		title={a.name}
		intro={`${a.id} · ${a.status === 'active' ? m.admin_active() : m.admin_inactive()}`}
	>
		<table class="list">
			<thead
				><tr
					><th>{m.service_token()}</th><th>{m.service_expires()}</th><th>{m.service_last_used()}</th
					><th><span class="visually-hidden">{m.ui_actions()}</span></th></tr
				></thead
			>
			<tbody>
				{#each tokens[a.id] ?? [] as t (t.id)}
					<tr>
						<td class="mono"
							>…{t.hint}{#if t.legacy_product}
								({t.legacy_product}){/if}</td
						>
						<td>{t.expires_at ? formatTime(t.expires_at) : m.service_never()}</td>
						<td>{formatTime(t.last_used_at)}</td>
						<td
							>{#if t.status === 'active'}<Button
									variant="quiet"
									disabled={busy}
									onclick={() => revoke(t)}>{m.service_revoke()}</Button
								>{:else}<span class="muted">{m.service_revoked_state()}</span>{/if}</td
						>
					</tr>
				{/each}
			</tbody>
		</table>
		<div class="row">
			<Button
				variant="secondary"
				disabled={busy || a.status !== 'active'}
				onclick={() => newToken(a)}>{m.service_new_token()}</Button
			>
			<Button variant="quiet" disabled={busy} onclick={() => toggle(a)}
				>{a.status === 'active' ? m.admin_disable() : m.admin_enable()}</Button
			>
			<Button variant="quiet" disabled={busy} onclick={() => remove(a)}>{m.admin_delete()}</Button>
		</div>
	</Section>
{/each}

<Section title={m.service_create()} intro={m.service_create_intro()}>
	<form class="stack" onsubmit={create}>
		<TextField label={m.field_name()} required bind:value={name} autocomplete="off" />
		<div><Button type="submit" variant="primary" {busy}>{m.service_create_submit()}</Button></div>
	</form>
</Section>

<style>
	.token {
		display: block;
		padding: var(--pid-space-3);
		background: var(--pid-surface-2);
		border-radius: var(--pid-radius);
		word-break: break-all;
		user-select: all;
	}
</style>
