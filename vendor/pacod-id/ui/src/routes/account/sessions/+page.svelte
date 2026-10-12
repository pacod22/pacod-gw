<script lang="ts">
	import { Button, Notice } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { del, get, post } from '#lib/api.ts';
	import { explain } from '#lib/errors.ts';
	import { formatTime } from '#lib/format.ts';
	import { m } from '#lib/paraglide/messages.js';
	import Section from '#lib/components/Section.svelte';

	interface SessionRow {
		id: string;
		current: boolean;
		device: string;
		ip: string;
		created_at: string;
		last_seen_at: string;
		expires_at: string;
		remember_me: boolean;
	}

	let sessions = $state<SessionRow[]>([]);
	let msg = $state<{ tone: 'success' | 'error'; text: string } | null>(null);
	let busy = $state(false);

	async function load() {
		sessions = (await get<{ sessions: SessionRow[] }>('account/sessions')).sessions;
	}
	onMount(load);

	async function end(s: SessionRow) {
		busy = true;
		try {
			await del(`account/sessions/${s.id}`);
			if (s.current) return window.location.assign('/account');
			await load();
		} catch (x) {
			msg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}

	async function endOthers() {
		busy = true;
		try {
			const r = await post<{ ended: number }>('account/sessions/others');
			msg = { tone: 'success', text: m.sessions_ended({ n: r.ended }) };
			await load();
		} catch (x) {
			msg = { tone: 'error', text: explain(x) };
		}
		busy = false;
	}
</script>

<svelte:head><title>{m.nav_sessions()}</title></svelte:head>

<h1>{m.nav_sessions()}</h1>

<Section title={m.sessions_title()} intro={m.sessions_intro()}>
	{#if msg}<Notice tone={msg.tone}>{msg.text}</Notice>{/if}
	<table class="list">
		<thead
			><tr
				><th>{m.sessions_device()}</th><th>{m.sessions_signed_in()}</th><th
					>{m.sessions_last_seen()}</th
				><th><span class="visually-hidden">{m.ui_actions()}</span></th></tr
			></thead
		>
		<tbody>
			{#each sessions as s (s.id)}
				<tr>
					<td
						>{s.device}{#if s.current}<strong class="this">({m.sessions_this()})</strong>{/if}<br
						/><span class="muted mono">{s.ip}</span></td
					>
					<td class="nowrap">{formatTime(s.created_at)}</td>
					<td class="nowrap">{formatTime(s.last_seen_at)}</td>
					<td
						><Button variant="quiet" disabled={busy} onclick={() => end(s)}
							>{s.current ? m.nav_sign_out() : m.sessions_end()}</Button
						></td
					>
				</tr>
			{/each}
		</tbody>
	</table>
	{#if sessions.length > 1}
		<p><Button variant="secondary" {busy} onclick={endOthers}>{m.sessions_end_others()}</Button></p>
	{/if}
</Section>

<style>
	.this {
		margin-left: 0.3em;
	}
</style>
