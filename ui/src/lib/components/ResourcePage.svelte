<script lang="ts" generics="T extends { id: string | number }">
	import type { Snippet } from 'svelte';
	import { onMount } from 'svelte';
	import type { ActionOutcome, ResourceData } from '#lib/server/resource.ts';
	import ActionDialog from './ActionDialog.svelte';
	import ConfirmDialog from './ConfirmDialog.svelte';
	import DataTable from './DataTable.svelte';
	import OutcomeNotice from './OutcomeNotice.svelte';
	import PageHeader from './PageHeader.svelte';
	import Pager from './Pager.svelte';
	import ProblemNotice from './ProblemNotice.svelte';
	import SecretDialog from './SecretDialog.svelte';
	import type { Column } from './table';

	/**
	 * The page of one resource: the list with paging, and dialogs to create, change and
	 * delete an object. It takes the data of `resource(...)` on the server and a column
	 * descriptor; everything else is optional.
	 */
	interface Props {
		title: string;
		description?: string;
		crumbs?: { label: string; href: string }[];
		/** What `load` of the resource kit returned. */
		data: ResourceData<T>;
		/** The page's `form` prop. */
		form: ActionOutcome | null | undefined;
		columns: Column<T>[];
		/** Shown when there is nothing yet: says how to create the first object. */
		empty: string;
		/** The name of an object in dialogs. Default: its `name`, `email`, `label` or `id`. */
		nameOf?: (item: T) => string;
		/** What deleting an object means, for the confirmation. */
		deleteNote?: string;
		/** More actions per row, shown before Edit and Delete. */
		rowActions?: Snippet<[T]>;
		/** Renders the cells of the columns named in `custom`. */
		cell?: Snippet<[T, Column<T>]>;
		custom?: string[];
		/** Content between the heading and the list. */
		children?: Snippet;
		/** Content at the top of the create and edit dialogs, inside the form. */
		formIntro?: Snippet<['create' | 'replace']>;
		/** A page that creates an object, for resources whose form is not the generic one. */
		createHref?: string;
		/** A page that edits an object, likewise. */
		editHref?: (item: T) => string;
		/** Objects for which Delete is offered. Default: all. */
		deletable?: (item: T) => boolean;
		/** Actions without a dialog of their own: their failures are shown above the list. */
		pageActions?: string[];
		/** Values the create form starts with; when given, the dialog opens with the page. */
		createInitial?: Record<string, unknown> | null;
	}

	let {
		title,
		description,
		crumbs,
		data,
		form,
		columns,
		empty,
		nameOf,
		deleteNote,
		rowActions,
		cell,
		custom,
		children,
		formIntro,
		createHref,
		editHref,
		deletable,
		pageActions,
		createInitial = null
	}: Props = $props();

	const noun = $derived(data.noun);

	function name(item: T): string {
		if (nameOf) return nameOf(item);
		const record = item as Record<string, unknown>;
		for (const key of ['name', 'email', 'label']) {
			const value = record[key];
			if (typeof value === 'string' && value) return value;
		}
		return String(item.id);
	}

	function versionOf(item: T): number | undefined {
		const version = (item as { version?: unknown }).version;
		return typeof version === 'number' ? version : undefined;
	}

	let creating = $state(false);
	onMount(() => {
		if (createInitial && data.createFields) creating = true;
	});
	// A copy of the object as it was when its dialog opened: a live update of the list must
	// not change a form someone is filling in, and the version sent back must be the one seen.
	let editing: T | null = $state(null);
	let deleting: T | null = $state(null);
	let editOpen = $state(false);
	let deleteOpen = $state(false);

	const canCreate = $derived(data.createFields !== null);
	const canEdit = $derived(data.replaceFields !== null);
	const hasRowActions = $derived(Boolean(rowActions) || canEdit || data.canDelete);
</script>

<div class="stack-lg">
	<PageHeader {title} {description} {crumbs}>
		{#snippet actions()}
			{@render addButton()}
		{/snippet}
	</PageHeader>

	<OutcomeNotice outcome={form} always={pageActions} />
	{@render children?.()}

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if data.items.length === 0}
		<div class="empty card">
			{#if data.cursor}
				<p>There is nothing on this page.</p>
			{:else}
				<p>{empty}</p>
				{@render addButton()}
			{/if}
		</div>
	{:else}
		<DataTable
			{columns}
			rows={data.items}
			caption={title}
			actions={hasRowActions ? rowButtons : undefined}
			{cell}
			{custom}
		/>
	{/if}
	<Pager cursor={data.cursor} nextCursor={data.nextCursor} count={data.items.length} />
</div>

{#snippet addButton()}
	{#if canCreate && createHref}
		<a class="btn btn-primary" href={createHref}>Add {noun}</a>
	{:else if canCreate}
		<button type="button" class="btn btn-primary" onclick={() => (creating = true)}>
			Add {noun}
		</button>
	{/if}
{/snippet}

{#snippet rowButtons(item: T)}
	{@render rowActions?.(item)}
	{#if canEdit && editHref}
		<a class="btn btn-quiet" href={editHref(item)}>
			Edit<span class="visually-hidden"> {name(item)}</span>
		</a>
	{:else if canEdit}
		<button
			type="button"
			class="btn btn-quiet"
			onclick={() => {
				editing = $state.snapshot(item) as T;
				editOpen = true;
			}}
		>
			Edit<span class="visually-hidden"> {name(item)}</span>
		</button>
	{/if}
	{#if data.canDelete && (deletable?.(item) ?? true)}
		<button
			type="button"
			class="btn btn-quiet danger"
			onclick={() => {
				deleting = $state.snapshot(item) as T;
				deleteOpen = true;
			}}
		>
			Delete<span class="visually-hidden"> {name(item)}</span>
		</button>
	{/if}
{/snippet}

{#if data.createFields && !createHref}
	<ActionDialog
		bind:open={creating}
		title="Add {noun}"
		action="?/create"
		submit="Create {noun}"
		outcome={form}
		fields={data.createFields}
		object={createInitial ?? undefined}
		mode="create"
	>
		{@render formIntro?.('create')}
	</ActionDialog>
{/if}

{#if data.replaceFields && !editHref}
	<ActionDialog
		bind:open={editOpen}
		title={editing ? `Edit ${noun} “${name(editing)}”` : `Edit ${noun}`}
		action="?/replace"
		submit="Save changes"
		outcome={form}
		fields={editing ? data.replaceFields : []}
		object={editing}
		mode="replace"
		hidden={{ id: editing?.id, version: editing ? versionOf(editing) : undefined }}
	>
		{@render formIntro?.('replace')}
	</ActionDialog>
{/if}

{#if data.canDelete}
	<ConfirmDialog
		bind:open={deleteOpen}
		title="Delete {noun}"
		action="?/delete"
		confirm="Delete {noun}"
		outcome={form}
		hidden={{ id: deleting?.id, name: deleting ? name(deleting) : undefined }}
	>
		<p>
			Delete the {noun} <strong>{deleting ? name(deleting) : ''}</strong>? This cannot be undone.
		</p>
		{#if deleteNote}<p class="muted">{deleteNote}</p>{/if}
	</ConfirmDialog>
{/if}

<SecretDialog outcome={form} />

<style>
	.empty {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--space-3);
		padding: var(--space-5);
		color: var(--text-muted);
	}
</style>
