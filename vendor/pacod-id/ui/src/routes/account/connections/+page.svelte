<script lang="ts">
	import { Button, Notice } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { del, get } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { formatTime } from '#lib/format.ts';
	import { m } from '#lib/paraglide/messages.js';
	import Section from '#lib/components/Section.svelte';

	interface Connections {
		password: boolean;
		google: { email: string; linked_at: string; last_used_at: string | null } | null;
		google_offered: boolean;
	}

	let c = $state<Connections | null>(null);
	let msg = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	let busy = $state(false);

	const load = async () => (c = await get<Connections>('account/connections'));
	onMount(load);

	async function unlink() {
		busy = true;
		try {
			await del('account/connections/google');
			msg = { tone: 'success', text: m.connections_unlinked() };
			await load();
		} catch (x) {
			msg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}
</script>

<svelte:head><title>{m.nav_connections()}</title></svelte:head>

<h1>{m.nav_connections()}</h1>

<Section title={m.connections_title()} intro={m.connections_intro()}>
	{#if msg}<Notice tone={msg.tone}>{msg.text}</Notice>{/if}
	{#if c}
		<table class="list">
			<tbody>
				<tr
					><th>{m.field_password()}</th><td
						>{c.password ? m.connections_password_set() : m.connections_password_none()}</td
					><td></td></tr
				>
				{#if c.google || c.google_offered}
					<tr>
						<th>Google</th>
						<td>
							{#if c.google}{m.connections_google_linked({ email: c.google.email })}<br /><span
									class="muted small"
									>{m.connections_last_used({ when: formatTime(c.google.last_used_at) })}</span
								>
							{:else}{m.connections_google_none()}{/if}
						</td>
						<td
							>{#if c.google}<Button variant="quiet" {busy} onclick={unlink}
									>{m.connections_unlink()}</Button
								>{/if}</td
						>
					</tr>
				{/if}
			</tbody>
		</table>
	{/if}
</Section>
