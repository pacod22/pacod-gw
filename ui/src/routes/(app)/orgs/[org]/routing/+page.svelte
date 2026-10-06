<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import { ALGORITHMS } from '#lib/routing.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const base = $derived(organisationPath(params.org, 'routing'));
</script>

<ResourcePage
	title="Routing schemes of {organisation}"
	description="A template can hold several models. A routing scheme decides, per call, which of them serves first and which take over when it fails: by the complexity, agent, task and tags the caller states. Assign a scheme to the organisation, a workspace or a template; the most specific one applies."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code', href: (s) => `${base}/${s.id}` },
		{ key: 'description', label: 'Description', wrap: true },
		{ key: 'rules', label: 'Rules', kind: 'number', value: (s) => s.rules.length },
		{
			key: 'default',
			label: 'Otherwise',
			value: (s) =>
				`${s.default.tiers?.length ? s.default.tiers.join(', ') : 'whole pool'} · ${ALGORITHMS.find((a) => a.value === s.default.algorithm)?.label ?? s.default.algorithm}`
		},
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'updated_at', label: 'Changed (UTC)', kind: 'datetime' }
	]}
	empty="This organisation has no routing schemes. Without one, a template with several models tries them by priority."
	deleteNote="A scheme cannot be deleted while the organisation, a workspace or a template names it."
	createHref="{base}/new"
	editHref={(s) => `${base}/${s.id}`}
/>
