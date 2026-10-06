<script lang="ts">
	import { enhance } from '$app/forms';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import TemplateEditor from '#lib/components/TemplateEditor.svelte';
	import { workspacePath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();
	const list = $derived(workspacePath(params.org, params.ws, 'templates'));
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);
	let busy = $state(false);
</script>

<div class="stack-lg editor">
	<PageHeader
		title="Add template"
		description="Choose the model, then decide for each parameter whether the template has an opinion: leave it to the caller, never send it, or set a value."
		crumbs={[{ label: 'Templates', href: list }]}
	/>

	{#if !data.context.writable}
		<ProblemNotice
			problem={{
				status: 403,
				type: 'forbidden',
				title: 'Your role does not allow this',
				message: 'Templates are created by administrators of the workspace or of its organisation.',
				hint: '',
				placement: 'notice',
				tone: 'warning',
				reload: false,
				fields: {},
				lists: []
			}}
		/>
	{:else}
		<form
			method="POST"
			action="?/save"
			class="stack"
			use:enhance={() => {
				busy = true;
				return async ({ update }) => {
					await update({ reset: false });
					busy = false;
				};
			}}
		>
			{#if problem}<ProblemNotice {problem} />{/if}
			<TemplateEditor
				models={data.context.models}
				tagKeys={data.context.tagKeys}
				schemes={data.context.schemes}
				inherited={data.context.inherited}
				fields={data.context.fields}
				{problem}
			/>
			<div class="row">
				<button type="submit" class="btn btn-primary" disabled={busy}>
					{busy ? 'Creating…' : 'Create template'}
				</button>
				<a class="btn" href={list}>Cancel</a>
			</div>
		</form>
	{/if}
</div>

<style>
	.editor {
		max-width: 52rem;
	}
</style>
