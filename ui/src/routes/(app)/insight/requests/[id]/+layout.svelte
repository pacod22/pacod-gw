<script lang="ts">
	import { canReadAuditRecords } from '#lib/access.ts';
	import Tabs from '#lib/components/Tabs.svelte';
	import type { LayoutProps } from './$types';

	let { data, children, params }: LayoutProps = $props();

	const base = $derived(`/insight/requests/${encodeURIComponent(params.id)}`);
	// The audit record holds prompts and answers: the tab is for those who may read them.
	const tabs = $derived([
		{ label: 'Call', href: base },
		...(canReadAuditRecords(data.principal)
			? [{ label: 'Audit record', href: `${base}/audit` }]
			: [])
	]);
</script>

<div class="stack-lg">
	<Tabs label="Request" {tabs} />
	{@render children()}
</div>
