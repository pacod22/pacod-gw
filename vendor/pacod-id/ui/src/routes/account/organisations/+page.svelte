<script lang="ts">
	import { Button, Notice } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { get, post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { formatTime } from '#lib/format.ts';
	import { m } from '#lib/paraglide/messages.js';
	import Section from '#lib/components/Section.svelte';

	interface Role {
		binding: string;
		role: string;
		product: string;
		scope: string;
		path: { scope: string; name: string }[];
		since: string;
	}
	interface Invitation {
		id: string;
		role_label: string;
		scope_label: string;
		expires_at: string;
	}

	let roles = $state<Role[]>([]);
	let invitations = $state<Invitation[]>([]);
	let msg = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	let busy = $state(false);

	async function load() {
		[roles, invitations] = await Promise.all([
			get<{ roles: Role[] }>('account/organisations').then((r) => r.roles),
			get<{ invitations: Invitation[] }>('account/invitations').then((r) => r.invitations)
		]);
	}
	onMount(load);

	const where = (r: Role) =>
		r.path
			.map((p) => p.name)
			.filter(Boolean)
			.join(' › ') || m.orgs_platform();
	const roleName = (r: Role) => r.role.split('.').slice(1).join('.').replaceAll('_', ' ');

	async function answer(inv: Invitation, accept: boolean) {
		busy = true;
		try {
			await post(`account/invitations/${inv.id}/${accept ? 'accept' : 'decline'}`);
			msg = { tone: 'success', text: accept ? m.invite_accepted() : m.orgs_declined() };
			await load();
		} catch (x) {
			msg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}
</script>

<svelte:head><title>{m.nav_organisations()}</title></svelte:head>

<h1>{m.nav_organisations()}</h1>

{#if msg}<Notice tone={msg.tone}>{msg.text}</Notice>{/if}

{#if invitations.length}
	<Section title={m.orgs_invitations()}>
		<table class="list">
			<tbody>
				{#each invitations as inv (inv.id)}
					<tr>
						<td
							><strong>{inv.scope_label}</strong> · {inv.role_label}<br /><span class="muted small"
								>{m.orgs_valid_until({ when: formatTime(inv.expires_at) })}</span
							></td
						>
						<td class="row">
							<Button disabled={busy} onclick={() => answer(inv, true)}>{m.invite_accept()}</Button>
							<Button variant="quiet" disabled={busy} onclick={() => answer(inv, false)}
								>{m.orgs_decline()}</Button
							>
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</Section>
{/if}

<Section title={m.orgs_roles()} intro={roles.length ? undefined : m.orgs_none()}>
	{#if roles.length}
		<table class="list">
			<thead
				><tr
					><th>{m.orgs_where()}</th><th>{m.orgs_role()}</th><th>{m.orgs_product()}</th><th
						>{m.orgs_since()}</th
					></tr
				></thead
			>
			<tbody>
				{#each roles as r (r.binding)}
					<tr
						><td>{where(r)}</td><td>{roleName(r)}</td><td class="mono">{r.product}</td><td
							>{formatTime(r.since)}</td
						></tr
					>
				{/each}
			</tbody>
		</table>
	{/if}
</Section>
