<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { limitAmount } from '#lib/limits.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();
	let resetting = $state<(Schemas['LimitSpec'] & { id: string }) | null>(null);
	let resetOpen = $state(false);

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
</script>

<ResourcePage
	title="Limits of {organisation}"
	description="Caps on what the whole organisation may consume. They apply on top of the limits of its workspaces: every limit that applies to a call must have room. Organisation limits are set by platform administrators."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code' },
		{
			key: 'amount',
			label: 'Allowance',
			value: (l) => limitAmount(l, data.principal.reporting_currency)
		},
		{ key: 'subject', label: 'Counts', value: (l) => l.subject.replaceAll('_', ' ') },
		{
			key: 'enforcement',
			label: 'When used up',
			value: (l) => (l.enforcement === 'request' ? 'Requests are rejected' : 'Reported only')
		},
		{ key: 'alert_thresholds', label: 'Alerts at %', kind: 'list' },
		{ key: 'status', label: 'Status', kind: 'status' }
	]}
	empty="This organisation has no cap. Add a limit to cap its tokens, requests or spend."
	deleteNote="The cap no longer applies. Its counters are discarded."
>
	{#snippet rowActions(item)}
		{#if data.canDelete}
			<button
				type="button"
				class="btn btn-quiet danger"
				onclick={() => {
					resetting = item;
					resetOpen = true;
				}}
			>
				Reset<span class="visually-hidden"> {item.name}</span>
			</button>
		{/if}
	{/snippet}
</ResourcePage>

<ConfirmDialog
	bind:open={resetOpen}
	title="Reset limit “{resetting?.name ?? ''}”"
	action="?/reset"
	confirm="Reset counters"
	outcome={form}
	hidden={{ id: resetting?.id, name: resetting?.name }}
>
	<p>
		The counters of this cap start again from zero for the whole organisation. What was used is not
		billed differently; only the limit forgets it.
	</p>
</ConfirmDialog>
