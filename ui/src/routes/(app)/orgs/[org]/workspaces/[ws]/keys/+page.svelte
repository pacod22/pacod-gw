<script lang="ts">
	import { page } from '$app/state';
	import type { ApiKey } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);
	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);

	let target: ApiKey | null = $state(null);
	let rotating = $state(false);
	let revoking = $state(false);
</script>

<ResourcePage
	createInitial={page.url.searchParams.has('new') ? {} : null}
	title="API keys of {workspace}"
	description="Applications call the gateway with these keys, one key per calling service. A key's secret is shown once, when the key is created or rotated."
	crumbs={[{ label: organisation, href: organisationPath(params.org) }]}
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name' },
		{ key: 'hint', label: 'Secret ends in', kind: 'code', value: (key) => `…${key.hint}` },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'allowed_modes', label: 'Modes', kind: 'list' },
		{ key: 'allowed_models', label: 'Models', kind: 'list', empty: 'All of the workspace' },
		{ key: 'expires_at', label: 'Expires (UTC)', kind: 'datetime', empty: 'Never' },
		{
			key: 'previous_secret_valid_until',
			label: 'Old secret until (UTC)',
			kind: 'datetime'
		}
	]}
	empty="This workspace has no API keys yet. Add a key for the first service that will call the gateway."
	deleteNote="Requests with this key are refused at once. To keep the key in the list for reference, revoke it instead."
>
	{#snippet rowActions(item)}
		{#if data.canDelete && item.status === 'active'}
			<button
				type="button"
				class="btn btn-quiet"
				onclick={() => {
					target = item;
					rotating = true;
				}}
			>
				Rotate<span class="visually-hidden"> {item.name}</span>
			</button>
			<button
				type="button"
				class="btn btn-quiet danger"
				onclick={() => {
					target = item;
					revoking = true;
				}}
			>
				Revoke<span class="visually-hidden"> {item.name}</span>
			</button>
		{/if}
	{/snippet}
</ResourcePage>

<ActionDialog
	bind:open={rotating}
	title="Rotate API key “{target?.name ?? ''}”"
	action="?/rotate"
	submit="Create new secret"
	outcome={form}
	fields={data.rotateFields}
	hidden={{ id: target?.id }}
>
	<p>
		The key gets a new secret, which is shown once. The current secret keeps working for the grace
		period, so that the calling service can be switched over without an interruption.
	</p>
</ActionDialog>

<ConfirmDialog
	bind:open={revoking}
	title="Revoke API key"
	action="?/revoke"
	confirm="Revoke key"
	outcome={form}
	hidden={{ id: target?.id }}
>
	<p>
		Revoke the API key <strong>{target?.name ?? ''}</strong>? Requests with it are refused within
		two seconds. A revoked key cannot be made active again; create a new key instead.
	</p>
</ConfirmDialog>
