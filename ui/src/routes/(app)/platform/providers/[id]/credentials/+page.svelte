<script lang="ts">
	import type { Credential } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { copyTargets } from '#lib/credentials.ts';
	import type { Field } from '#lib/schema/form.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const provider = $derived(data.provider?.name ?? 'provider');

	function scopeOf(credential: Credential): string {
		const scope = credential.scope;
		if (scope.type === 'global') return 'All workspaces';
		const named = data.targets?.find((t) => t.type === scope.type && t.id === scope.id);
		return named?.label ?? `${scope.type} ${scope.id}`;
	}

	const nameOf = (credential: Credential) => credential.label || credential.fingerprint;

	// A service account's key is a JSON file: it can be chosen instead of pasted. It is read in
	// this browser and sent once with the form, like a typed key.
	const serviceAccount = $derived(data.provider?.auth === 'google_service_account');
	let keyFile = $state<{ ok: boolean; text: string } | null>(null);

	async function readKeyFile(event: Event & { currentTarget: HTMLInputElement }) {
		const input = event.currentTarget;
		const file = input.files?.[0];
		const secret = input.form?.elements.namedItem('secret');
		keyFile = null;
		if (!file || !(secret instanceof HTMLInputElement)) return;
		if (file.size > 16 * 1024) {
			keyFile = {
				ok: false,
				text: 'This file is larger than a service account key (16 KiB at most).'
			};
			return;
		}
		try {
			const key = JSON.parse(await file.text()) as Record<string, unknown>;
			if (key.type !== 'service_account' || typeof key.client_email !== 'string') {
				keyFile = { ok: false, text: 'This file is not a service account key.' };
				return;
			}
			secret.value = JSON.stringify(key);
			const id = typeof key.private_key_id === 'string' ? key.private_key_id.slice(0, 8) : '';
			keyFile = {
				ok: true,
				text: `The key of ${key.client_email}${id ? ` (key ${id})` : ''} is ready to be added.`
			};
		} catch {
			keyFile = { ok: false, text: 'This file is not JSON.' };
		} finally {
			// The file itself is not sent: only the key, in the secret field.
			input.value = '';
		}
	}

	let target = $state<Credential | null>(null);
	let makingCurrent = $state(false);
	let disabling = $state(false);
	let giving = $state(false);

	const giveFields = $derived<Field[]>(
		target
			? [
					{
						kind: 'enum',
						key: 'to',
						path: 'to',
						label: 'Give to',
						help: 'The organisation or workspace that gets a copy of the key as its own.',
						required: true,
						nullable: false,
						options: copyTargets(target, data.targets ?? [], data.items).map((t) => ({
							value: `${t.type}:${t.id}`,
							label: t.label,
							help: ''
						}))
					},
					...(data.copyFields ?? [])
				]
			: []
	);
	const sharedKey = $derived(target?.scope.type === 'global' && target.state === 'current');
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
	{#snippet formIntro(mode)}
		{#if mode === 'create' && serviceAccount}
			<div class="field key-file">
				<label for="key-file">Service account key file</label>
				<input id="key-file" type="file" accept=".json,application/json" onchange={readKeyFile} />
				<p class="help" class:error={keyFile && !keyFile.ok}>
					{keyFile?.text ??
						"This provider signs in with a Google Cloud service account: choose its JSON key file, or paste the file's content as the secret. The gateway stores it encrypted, shows only the account and the key id, and gets access tokens with it."}
				</p>
			</div>
		{/if}
	{/snippet}
	{#snippet rowActions(item)}
		{#if data.canDelete && item.state !== 'disabled' && copyTargets(item, data.targets ?? [], data.items).length > 0}
			<button
				type="button"
				class="btn btn-quiet"
				onclick={() => {
					target = item;
					giving = true;
				}}
			>
				Give to…<span class="visually-hidden">: {nameOf(item)}</span>
			</button>
		{/if}
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
	bind:open={giving}
	title="Give credential to an organisation or a workspace"
	action="?/copy"
	submit="Give key"
	outcome={form}
	fields={giveFields}
	hidden={{ id: target?.id }}
>
	<p>
		The organisation or workspace gets a copy of <strong>{target ? nameOf(target) : ''}</strong> as
		its own key for {provider}; the secret is not entered again and never leaves the gateway. Made
		current, the copy replaces its key for {provider}, which stays usable for the grace period.
		Rotating this credential does not change the copy.
	</p>
	{#if sharedKey}
		<p class="muted">
			This is the installation’s shared key. As an organisation’s or a workspace’s own key it lets
			that organisation’s or workspace’s calls reach files, batches and the blind relay on the
			provider account that every workspace shares.
		</p>
	{/if}
</ActionDialog>

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

<style>
	.key-file {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--space-1);
		padding: var(--space-3);
		border: 1px dashed var(--border-strong);
		border-radius: var(--radius);
	}
	.key-file .help {
		color: var(--text-muted);
		font-size: var(--text-sm, 0.875rem);
	}
	.key-file .error {
		color: var(--danger);
	}
</style>
