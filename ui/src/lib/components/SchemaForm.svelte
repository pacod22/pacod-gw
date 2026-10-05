<script lang="ts">
	import type { ApiProblem } from '#lib/api/errors.ts';
	import type { Field } from '#lib/schema/form.ts';
	import SchemaField from './SchemaField.svelte';

	/**
	 * The controls of a form, rendered from the fields that `buildFields` derived from the
	 * resource's JSON Schema. The surrounding `<form>` and its buttons belong to the caller.
	 */
	interface Props {
		fields: Field[];
		/** The object whose values the form shows; none for a new object. */
		object?: unknown;
		/** The failure of the last submission: messages are shown at the fields they name. */
		problem?: ApiProblem | null;
		/** Keeps the ids of two forms on one page apart. */
		idPrefix: string;
		mode?: 'create' | 'replace';
	}

	let { fields, object, problem = null, idPrefix, mode = 'create' }: Props = $props();
</script>

<div class="fields">
	{#each fields as field (field.path)}
		<SchemaField {field} {object} errors={problem?.fields ?? {}} {idPrefix} {mode} />
	{/each}
</div>

<style>
	.fields {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
	}
</style>
