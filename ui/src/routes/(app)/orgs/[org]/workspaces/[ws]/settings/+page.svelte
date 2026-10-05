<script lang="ts">
	import ObjectPage from '#lib/components/ObjectPage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const name = $derived(data.item?.display_name || data.item?.name || params.ws);
</script>

<ObjectPage
	title="Settings of {name}"
	description="The workspace itself: its name, whether it is active, the tag keys its requests may carry, and the settings it overrides. A setting that is not set here is inherited from the organisation, then from the installation."
	crumbs={[{ label: organisation, href: organisationPath(params.org) }]}
	{data}
	{form}
/>
