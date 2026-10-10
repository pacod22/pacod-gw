<script lang="ts">
	import type { Field } from '#lib/schema/form.ts';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import type { OwnCredential } from '#lib/server/credentials.ts';
	import ActionDialog from './ActionDialog.svelte';
	import ConfirmDialog from './ConfirmDialog.svelte';
	import DataTable from './DataTable.svelte';

	/**
	 * The provider keys an organisation or a workspace brings itself, as a section of its
	 * settings page. Only its admins see them; a key is shown by its fingerprint, never again
	 * in full.
	 */
	interface Props {
		noun: 'organisation' | 'workspace';
		credentials: OwnCredential[];
		fields: Field[];
		mayManage: boolean;
		form: ActionOutcome | null | undefined;
	}

	let { noun, credentials, fields, mayManage, form }: Props = $props();

	const nameOf = (c: OwnCredential) => c.label || c.fingerprint;
	let target: OwnCredential | null = $state(null);
	let adding = $state(false);
	let makingCurrent = $state(false);
	let disabling = $state(false);
	let deleting = $state(false);
	/** What a dialog sends along to name the key it is about. */
	const keyOf = (c: OwnCredential | null) => ({ id: c?.id, provider_id: c?.provider_id });
</script>

<section class="card stack section" id="credentials" aria-labelledby="credentials-title">
	<header class="stack-sm">
		<h2 id="credentials-title">Provider keys</h2>
		<p class="muted small">
			{#if noun === 'organisation'}
				Keys of the organisation’s own provider accounts. Its workspaces’ calls to that provider use
				them instead of the installation’s shared key, unless a workspace has a key of its own.
				Proxy calls that name no model the gateway checks (files, batches, a blind relay) need such
				a key.
			{:else}
				Keys of the workspace’s own provider accounts. Its calls to that provider use them before
				the organisation’s key and the installation’s shared one. Proxy calls that name no model the
				gateway checks (files, batches, a blind relay) need such a key.
			{/if}
		</p>
	</header>
	{#if !mayManage}
		<p class="muted">Only the {noun}’s admins see and manage its provider keys.</p>
	{:else}
		{#if credentials.length === 0}
			<p class="muted">
				The {noun} has no keys of its own: its calls use {noun === 'workspace'
					? 'the organisation’s key or '
					: ''}the installation’s shared key.
			</p>
		{:else}
			<DataTable
				caption="Provider keys"
				rows={credentials}
				columns={[
					{ key: 'provider_name', label: 'Provider' },
					{ key: 'label', label: 'Label', empty: 'No label' },
					{ key: 'fingerprint', label: 'Fingerprint', kind: 'code' },
					{ key: 'state', label: 'State', kind: 'status' },
					{ key: 'retire_at', label: 'Kept until (UTC)', kind: 'datetime' },
					{ key: 'created_at', label: 'Added (UTC)', kind: 'datetime' }
				]}
				actions={rowActions}
				menu={(c) => `Actions for ${nameOf(c)}`}
			/>
		{/if}
		<div class="row">
			<button type="button" class="btn" onclick={() => (adding = true)}>Add key</button>
		</div>
	{/if}
</section>

{#snippet rowActions(item: OwnCredential)}
	{#if item.state !== 'current'}
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
	{#if item.state !== 'disabled'}
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
	<button
		type="button"
		class="btn btn-quiet danger"
		onclick={() => {
			target = item;
			deleting = true;
		}}
	>
		Delete<span class="visually-hidden">: {nameOf(item)}</span>
	</button>
{/snippet}

<ActionDialog
	bind:open={adding}
	title="Add a provider key"
	action="?/addCredential"
	submit="Add key"
	outcome={form}
	{fields}
>
	<p>
		The key is stored encrypted and never shown again; the list names it by a fingerprint. Made
		current, it replaces the {noun}’s key for that provider, which stays usable for the grace
		period.
	</p>
</ActionDialog>

<ActionDialog
	bind:open={makingCurrent}
	title="Make key current"
	action="?/credentialState"
	submit="Make current"
	outcome={form}
	hidden={{ ...keyOf(target), state: 'current' }}
>
	<p>
		Use <strong>{target ? nameOf(target) : ''}</strong> for the {noun}’s new requests to {target?.provider_name ??
			'the provider'}? The key it replaces becomes retiring.
	</p>
</ActionDialog>

<ConfirmDialog
	bind:open={disabling}
	title="Disable key"
	action="?/credentialState"
	confirm="Disable key"
	outcome={form}
	hidden={{ ...keyOf(target), state: 'disabled' }}
>
	<p>
		Take <strong>{target ? nameOf(target) : ''}</strong> out of use? While no key of the {noun}
		is current, its calls to {target?.provider_name ?? 'the provider'} fall back to {noun ===
		'workspace'
			? 'the organisation’s key or '
			: ''}the installation’s shared key.
	</p>
</ConfirmDialog>

<ConfirmDialog
	bind:open={deleting}
	title="Delete key"
	action="?/deleteCredential"
	confirm="Delete key"
	outcome={form}
	hidden={keyOf(target)}
>
	<p>
		Delete <strong>{target ? nameOf(target) : ''}</strong>? The encrypted key is removed. Files and
		batches created with it can no longer be reached through the gateway.
	</p>
</ConfirmDialog>
