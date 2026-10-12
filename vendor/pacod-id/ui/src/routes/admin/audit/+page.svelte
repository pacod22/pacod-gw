<script lang="ts">
	import { Button, TextField } from '@pacod/ui';
	import { onMount } from 'svelte';
	import { get } from '#lib/api.ts';
	import { formatTime } from '#lib/format.ts';
	import { m } from '#lib/paraglide/messages.js';
	import Section from '#lib/components/Section.svelte';

	interface Entry {
		id: number;
		at: string;
		actor: string;
		actor_kind: string;
		action: string;
		subject: string;
		realm: string | null;
		ip: string;
	}

	let action = $state('');
	let actor = $state('');
	let subject = $state('');
	let entries = $state<Entry[]>([]);
	let next = $state<number | null>(null);

	async function load(before?: number) {
		const filters: [string, string | number | undefined][] = [
			['limit', 100],
			['action', action],
			['actor', actor],
			['subject', subject],
			['before', before]
		];
		const query = filters
			.filter(([, v]) => v !== undefined && v !== '')
			.map(([k, v]) => `${k}=${encodeURIComponent(String(v))}`)
			.join('&');
		const r = await get<{ entries: Entry[]; next: number | null }>(`admin/audit?${query}`);
		entries = before ? [...entries, ...r.entries] : r.entries;
		next = r.next;
	}
	onMount(() => load());

	/** Ids are long: the start is enough to tell them apart; the whole one is on hover. */
	const short = (id: string) => (id.length > 18 ? `${id.slice(0, 16)}…` : id);
</script>

<svelte:head><title>{m.nav_admin_audit()}</title></svelte:head>

<h1>{m.nav_admin_audit()}</h1>

<Section title={m.audit_title()}>
	<form class="row" onsubmit={(e) => (e.preventDefault(), load())}>
		<TextField label={m.audit_action()} bind:value={action} />
		<TextField label={m.audit_actor()} bind:value={actor} />
		<TextField label={m.audit_subject()} bind:value={subject} />
		<Button type="submit" variant="secondary">{m.admin_search_submit()}</Button>
	</form>
	<table class="list">
		<thead
			><tr
				><th>{m.audit_when()}</th><th>{m.audit_action()}</th><th>{m.audit_actor()}</th><th
					>{m.audit_subject()}</th
				></tr
			></thead
		>
		<tbody>
			{#each entries as e (e.id)}
				<tr>
					<td class="nowrap">{formatTime(e.at)}</td>
					<td class="mono">{e.action}</td>
					<td class="mono" title={e.actor}
						>{e.actor ? short(e.actor) : '—'}<br /><span class="muted">{e.ip}</span></td
					>
					<td class="mono" title={e.subject}>{e.subject ? short(e.subject) : '—'}</td>
				</tr>
			{/each}
		</tbody>
	</table>
	{#if next}<p>
			<Button variant="secondary" onclick={() => load(next ?? undefined)}>{m.ui_more()}</Button>
		</p>{/if}
</Section>
