<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { quantity } from '#lib/limits.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);
	const hasEndUserLimits = $derived(Object.keys(data.limits).length > 0);
</script>

<ResourcePage
	title="End-user overrides of {workspace}"
	description="A different allowance for one end user, or an exemption, on a limit that counts each end user."
	{data}
	{form}
	nameOf={(override) => override.end_user}
	columns={[
		{ key: 'end_user', label: 'End user' },
		{
			key: 'limit_id',
			label: 'Limit',
			kind: 'code',
			value: (o) => data.limits[o.limit_id]?.name ?? o.limit_id
		},
		{
			key: 'amount',
			label: 'Allowance',
			value: (o) =>
				o.exempt
					? 'Exempt: the limit does not apply'
					: typeof o.amount === 'number'
						? quantity(o.amount, data.limits[o.limit_id]?.metric ?? '')
						: ''
		},
		{ key: 'updated_at', label: 'Changed (UTC)', kind: 'datetime' }
	]}
	empty={hasEndUserLimits
		? 'No end user has an allowance of their own. Add an override to give one end user more, less, or an exemption.'
		: 'This workspace has no limit that counts each end user, so there is nothing to override. Add such a limit on the Limits tab first.'}
	deleteNote="The end user is counted against the limit's regular allowance again."
/>
