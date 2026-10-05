<script lang="ts">
	import type { Credential } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const provider = $derived(data.provider?.name ?? 'provider');

	function scopeOf(credential: Credential): string {
		const scope = credential.scope;
		return scope.type === 'global' ? 'All workspaces' : `${scope.type} ${scope.id}`;
	}

	const nameOf = (credential: Credential) => credential.label || credential.fingerprint;

	let target: Credential | null = $state(null);
	let makingCurrent = $state(false);
	let disabling = $state(false);
</script>

<ResourcePage
	title="Credentials of {provider}"
	description="The keys the gateway presents to this provider. To rotate a key, add the new one: it becomes current and the one it replaces is kept as retiring for the grace period, so that you can go back."
	crumbs={[{ label: 'Providers', href: '/platform/providers' }]}
	{data}
	{form}
	{nameOf}
	columns={[
		{ key: 'label', label: 'Label', empty: 'No label' },
		{ key: 'fingerprint', label: 'Fingerprint', kind: 'code' },
		{ key: 'scope', label: 'Used for', value: scopeOf },
		{ key: 'state', label: 'State', kind: 'status' },
		{ key: 'retire_at', label: 'Kept until (UTC)', kind: 'datetime' },
		{ key: 'created_at', label: 'Added (UTC)', kind: 'datetime' }
	]}
	empty="This provider has no credential yet. Add its key; the gateway stores it encrypted and shows only a fingerprint afterwards."
	deleteNote="The encrypted key is removed. If it is the current credential, requests to this provider fail until another one is made current."
>
	{#snippet rowActions(item)}
		{#if data.canDelete && item.state !== 'current'}
			<button
				type="button"
				class="btn btn-quiet"
				onclick={() => {
					target = item;
					makingCurrent = true;
				}}
			>
				Make current<span class="visually-hidden">: {nameOf(item)}</span>
			</button>
		{/if}
		{#if data.canDelete && item.state !== 'disabled'}
			<button
				type="button"
				class="btn btn-quiet danger"
				onclick={() => {
					target = item;
					disabling = true;
				}}
			>
				Disable<span class="visually-hidden">: {nameOf(item)}</span>
			</button>
		{/if}
	{/snippet}
</ResourcePage>

<ActionDialog
	bind:open={makingCurrent}
	title="Make credential current"
	action="?/state"
	submit="Make current"
	outcome={form}
	hidden={{ id: target?.id, state: 'current' }}
>
	<p>
		Use <strong>{target ? nameOf(target) : ''}</strong> for new requests to {provider} from now on? The
		credential it replaces becomes retiring.
	</p>
</ActionDialog>

<ConfirmDialog
	bind:open={disabling}
	title="Disable credential"
	action="?/state"
	confirm="Disable credential"
	outcome={form}
	hidden={{ id: target?.id, state: 'disabled' }}
>
	<p>
		Take <strong>{target ? nameOf(target) : ''}</strong> out of use?
		{#if target?.state === 'current'}
			It is the current credential: requests to {provider} fail until another one is made current.
		{:else}
			It can be made current again later.
		{/if}
	</p>
</ConfirmDialog>
