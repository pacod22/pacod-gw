<script lang="ts">
	import HolidayCalendars from '#lib/components/HolidayCalendars.svelte';
	import ObjectPage from '#lib/components/ObjectPage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	// The holiday calendars have an editor of their own: dates one per line, years checked.
	const calendars = $derived(
		(data.fields ?? data.readFields).find((f) => f.path === 'holiday_calendars')
	);
</script>

<ObjectPage
	title={data.topic.title}
	description={data.topic.description}
	{data}
	{form}
	omit={calendars ? ['holiday_calendars'] : []}
>
	{#snippet fields()}
		{#if calendars && data.item}
			<HolidayCalendars
				name="holiday_calendars"
				value={data.item.holiday_calendars}
				help={calendars.help}
			/>
		{/if}
	{/snippet}
</ObjectPage>
