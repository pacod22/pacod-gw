<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { limitAmount } from '#lib/limits.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

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
		{ key: 'amount', label: 'Allowance', value: limitAmount },
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
/>
