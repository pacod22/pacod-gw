<script lang="ts">
	import type { Snippet } from 'svelte';
	import { untrack } from 'svelte';
	import { enhance } from '$app/forms';
	import type { SubmitFunction } from '$app/forms';
	import type { Field } from '#lib/schema/form.ts';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import Dialog from './Dialog.svelte';
	import ProblemNotice from './ProblemNotice.svelte';
	import SchemaForm from './SchemaForm.svelte';

	/**
	 * A dialog around one form action: optional text, optional schema fields, Cancel and a
	 * button that says what it does. It shows the action's failure next to the form and
	 * closes when the action worked.
	 */
	interface Props {
		open: boolean;
		title: string;
		/** The form action, such as `?/create`. */
		action: string;
		/** The label of the submit button: what the action does. */
		submit: string;
		/** The page's `form` prop: the result of the last action. */
		outcome: ActionOutcome | null | undefined;
		fields?: Field[];
		/** The object the form starts from; none for a new object. */
		object?: unknown;
		mode?: 'create' | 'replace';
		/** Values sent along, such as the id and the version of the object. */
		hidden?: Record<string, string | number | undefined>;
		/** A destructive action: the button is marked as such. */
		danger?: boolean;
		/** Holds the button back until the form is ready, such as a typed confirmation. */
		ready?: boolean;
		children?: Snippet;
		onclose?: () => void;
	}

	let {
		open = $bindable(false),
		title,
		action,
		submit,
		outcome,
		fields = [],
		object,
		mode = 'create',
		hidden = {},
		danger = false,
		ready = true,
		children,
		onclose
	}: Props = $props();

	const uid = $props.id();
	const name = $derived(action.replace(/^\?\//, ''));
	let busy = $state(false);
	/** The result that was already there when the dialog opened: not this form's. */
	let before: unknown = $state(null);
	/** Counts openings: each one gets fresh fields, so nothing typed earlier remains. */
	let generation = $state(0);

	$effect(() => {
		if (open) {
			untrack(() => {
				before = outcome;
				generation += 1;
			});
		}
	});

	const problem = $derived(
		outcome && outcome !== before && !outcome.ok && outcome.action === name
			? (outcome.problem ?? null)
			: null
	);

	const submitting: SubmitFunction = () => {
		busy = true;
		return async ({ result, update }) => {
			await update();
			busy = false;
			if (result.type === 'success') open = false;
		};
	};
</script>

<Dialog bind:open {title} {onclose}>
	<form method="POST" {action} use:enhance={submitting} class="stack">
		{#each Object.entries(hidden) as [key, value] (key)}
			{#if value !== undefined}<input type="hidden" name={key} {value} />{/if}
		{/each}
		{#if problem}<ProblemNotice {problem} onreload={() => (open = false)} />{/if}
		{@render children?.()}
		{#if fields.length > 0}
			{#key generation}
				<SchemaForm {fields} {object} {mode} {problem} idPrefix={uid} />
			{/key}
		{/if}
		<div class="buttons">
			<button type="button" class="btn" onclick={() => (open = false)}>Cancel</button>
			<button
				type="submit"
				class="btn {danger ? 'btn-danger' : 'btn-primary'}"
				disabled={busy || !ready}
			>
				{busy ? 'Working…' : submit}
			</button>
		</div>
	</form>
</Dialog>

<style>
	.buttons {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: var(--space-2);
		padding-top: var(--space-2);
	}
</style>
