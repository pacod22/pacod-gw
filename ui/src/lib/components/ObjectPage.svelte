<script lang="ts" generics="T">
	import type { Snippet } from 'svelte';
	import { enhance } from '$app/forms';
	import type { ActionOutcome, ObjectData } from '#lib/server/resource.ts';
	import OutcomeNotice from './OutcomeNotice.svelte';
	import PageHeader from './PageHeader.svelte';
	import ProblemNotice from './ProblemNotice.svelte';
	import SchemaForm from './SchemaForm.svelte';

	/**
	 * The page of one object that is read and replaced as a whole: its form, rendered from
	 * the schema, and a Save button. Someone who may only read sees the values disabled.
	 */
	interface Props {
		title: string;
		description?: string;
		crumbs?: { label: string; href: string }[];
		/** What `load` of `objectResource(...)` returned. */
		data: ObjectData<T>;
		/** The page's `form` prop. */
		form: ActionOutcome | null | undefined;
		/** Content between the heading and the form. */
		children?: Snippet;
	}

	let { title, description, crumbs, data, form, children }: Props = $props();

	let busy = $state(false);

	const version = $derived.by(() => {
		const value = (data.item as { version?: unknown } | null)?.version;
		return typeof value === 'number' ? value : undefined;
	});
	const problem = $derived(
		form && !form.ok && form.action === 'save' && form.problem?.placement === 'form'
			? form.problem
			: null
	);
</script>

<div class="stack-lg">
	<PageHeader {title} {description} {crumbs} />
	<OutcomeNotice outcome={form} />
	{@render children?.()}

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if data.item}
		<form
			method="POST"
			action="?/save"
			class="card stack object"
			use:enhance={() => {
				busy = true;
				return async ({ update }) => {
					// Keep what was typed: after a save it is what the object now holds.
					await update({ reset: false });
					busy = false;
				};
			}}
		>
			{#if version !== undefined}<input type="hidden" name="version" value={version} />{/if}
			{#if problem}<ProblemNotice {problem} />{/if}
			<fieldset class="bare" disabled={data.fields === null}>
				<legend class="visually-hidden">{title}</legend>
				<SchemaForm
					fields={data.fields ?? data.readFields}
					object={data.item}
					mode="replace"
					{problem}
					idPrefix="object"
				/>
			</fieldset>
			{#if data.fields}
				<div class="row">
					<button type="submit" class="btn btn-primary" disabled={busy}>
						{busy ? 'Saving…' : 'Save changes'}
					</button>
				</div>
			{:else}
				<p class="muted small">Your role lets you read these values but not change them.</p>
			{/if}
		</form>
	{/if}
</div>

<style>
	.object {
		max-width: 44rem;
	}

	.bare {
		padding: 0;
		border: 0;
	}
</style>
