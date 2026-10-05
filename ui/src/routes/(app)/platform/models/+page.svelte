<script lang="ts">
	import { enhance } from '$app/forms';
	import { page } from '$app/state';
	import type { Schemas } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import Dialog from '#lib/components/Dialog.svelte';
	import PresetPicker from '#lib/components/PresetPicker.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { formatInteger } from '#lib/format.ts';
	import { stateChanges, stateHelp } from '#lib/models.ts';
	import type { Field } from '#lib/schema/form.ts';
	import type { PageProps } from './$types';

	type Model = Schemas['Item9'];

	/** What the dry run of a state change answered. */
	interface Preview {
		to: string;
		from: string;
		impact: Schemas['Impact'];
		model: { id: string; name: string; successor_id: string | null; sunset_at: string | null };
		fields: Field[];
	}

	let { data, form }: PageProps = $props();

	// Step one: which change. Step two: what it affects, then confirm.
	let choosing = $state<Model | null>(null);
	let chooseOpen = $state(false);
	let confirmOpen = $state(false);
	let preview = $state<Preview | null>(null);

	$effect(() => {
		if (form?.ok && form.action === 'preview' && form.data) {
			preview = form.data as Preview;
			chooseOpen = false;
			confirmOpen = true;
		}
	});

	const changes = $derived(choosing ? stateChanges(choosing.state, choosing.previous_state) : []);
	const chosen = $derived(
		preview ? stateChanges(preview.from).find((c) => c.to === preview?.to) : undefined
	);
	const affected = $derived(
		preview
			? [
					{ label: 'Organisations it is granted to', items: preview.impact.organisations },
					{ label: 'Workspaces that can use it', items: preview.impact.workspaces },
					{ label: 'Templates whose current version uses it', items: preview.impact.templates },
					{ label: 'API keys whose allow-list names it', items: preview.impact.keys },
					{ label: 'Models that name it as their successor', items: preview.impact.successor_of }
				]
			: []
	);

	// "Create draft" on a provider's discovery page leads here with the details in the query.
	const draft = $derived.by(() => {
		const query = page.url.searchParams;
		if (!query.has('draft')) return null;
		const number = (name: string) => (query.get(name) ? Number(query.get(name)) : null);
		return {
			name: query.get('name') ?? '',
			provider_id: query.get('provider_id') ?? '',
			upstream_id: query.get('upstream_id') ?? '',
			display_name: query.get('display_name') ?? '',
			context_window: number('context_window'),
			max_output_tokens: number('max_output_tokens')
		};
	});

	const presets = $derived(
		data.presets.map((p) => ({
			name: p.name,
			label: `${p.name} (${p.family})`,
			notes: p.description,
			values: p.capabilities as Record<string, unknown>
		}))
	);
</script>

<ResourcePage
	title="Models"
	description="The model repository: every model applications can ask for, the provider that serves it and what it accepts. A model starts as a draft, needs a price (or to be marked free) to become active, and is deprecated and retired rather than deleted."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Model', kind: 'code' },
		{ key: 'provider', label: 'Provider' },
		{ key: 'kind', label: 'Kind' },
		{ key: 'state', label: 'State', kind: 'status' },
		{ key: 'priced', label: 'Priced', value: (m) => (m.free ? 'Free' : m.priced ? 'Yes' : 'No') },
		{ key: 'successor', label: 'Successor', kind: 'code' },
		{ key: 'sunset_at', label: 'Sunset (UTC)', kind: 'datetime' }
	]}
	empty="No models yet. Add the first model as a draft, give it a price, then activate it."
	deleteNote="Only a draft can be deleted. A model that was in use is retired instead, so that its usage history keeps resolving."
	deletable={(model) => model.state === 'draft'}
	pageActions={['preview', 'probe']}
	createInitial={draft}
>
	{#snippet formIntro()}
		<PresetPicker
			label="Start the capability profile from a preset"
			help="Choosing a preset fills the capability fields below. They stay editable."
			{presets}
			prefix="capabilities"
		/>
	{/snippet}
	{#snippet rowActions(item)}
		{#if data.canDelete}
			<button
				type="button"
				class="btn btn-quiet"
				onclick={() => {
					choosing = item;
					chooseOpen = true;
				}}
			>
				Change state<span class="visually-hidden"> of {item.name}</span>
			</button>
			<form method="POST" action="?/probe" use:enhance>
				<input type="hidden" name="id" value={item.id} />
				<input type="hidden" name="name" value={item.name} />
				<button type="submit" class="btn btn-quiet">
					Probe<span class="visually-hidden"> {item.name}</span>
				</button>
			</form>
		{/if}
		<a class="btn btn-quiet" href="/platform/models/{item.id}/prices">
			Prices<span class="visually-hidden"> of {item.name}</span>
		</a>
	{/snippet}
</ResourcePage>

<Dialog bind:open={chooseOpen} title="Change the state of “{choosing?.name ?? ''}”">
	{#if choosing}
		<form method="POST" action="?/preview" use:enhance class="stack">
			<input type="hidden" name="id" value={choosing.id} />
			<p>
				The model is <strong>{choosing.state}</strong>. {stateHelp(choosing.state)}
			</p>
			<fieldset class="stack">
				<legend>Change it to</legend>
				{#each changes as change, index (change.to)}
					<label class="choice">
						<input type="radio" name="to" value={change.to} checked={index === 0} required />
						<span>
							<strong>{change.label}</strong>
							<span class="muted">{change.help}</span>
						</span>
					</label>
				{/each}
			</fieldset>
			<p class="muted small">Nothing changes yet: the next step shows what the change affects.</p>
			<div class="buttons">
				<button type="button" class="btn" onclick={() => (chooseOpen = false)}>Cancel</button>
				<button type="submit" class="btn btn-primary">Show what this affects</button>
			</div>
		</form>
	{/if}
</Dialog>

<ActionDialog
	bind:open={confirmOpen}
	title="{chosen?.label ?? 'Change state'}: {preview?.model.name ?? ''}"
	action="?/transition"
	submit={chosen?.label ?? 'Apply'}
	outcome={form}
	fields={preview?.fields ?? []}
	object={preview?.model}
	mode="replace"
	hidden={{ id: preview?.model.id, to: preview?.to, from: preview?.from }}
	danger={preview?.to === 'retired' || preview?.to === 'disabled'}
>
	{#if preview}
		<p>
			From <strong>{preview.from}</strong> to <strong>{preview.to}</strong>. {stateHelp(preview.to)}
		</p>
		<div class="impact">
			<h3>What this affects</h3>
			<dl class="facts">
				{#each affected as group (group.label)}
					<dt>{group.label}</dt>
					<dd>
						{#if group.items.length > 0}
							<strong>{group.items.length}</strong>
							<span class="muted">– {group.items.join(', ')}</span>
						{:else}
							None
						{/if}
					</dd>
				{/each}
				<dt>Calls in the last 30 days</dt>
				<dd>
					{#if preview.impact.requests_last_30d === null || preview.impact.requests_last_30d === undefined}
						<span class="muted">Not known: the analytics service cannot be asked.</span>
					{:else}
						{formatInteger(preview.impact.requests_last_30d)}
					{/if}
				</dd>
			</dl>
		</div>
	{/if}
</ActionDialog>

<style>
	.choice {
		display: flex;
		align-items: baseline;
		gap: var(--space-2);
		font-weight: 400;
	}

	.choice span {
		display: flex;
		flex-direction: column;
	}

	.buttons {
		display: flex;
		justify-content: flex-end;
		gap: var(--space-2);
	}

	.impact {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		padding: var(--space-3);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--surface-2);
	}
</style>
