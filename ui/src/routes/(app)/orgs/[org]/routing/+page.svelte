<script lang="ts">
	import { enhance } from '$app/forms';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import { ALGORITHMS } from '#lib/routing.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const base = $derived(organisationPath(params.org, 'routing'));
	const assignment = $derived(data.assignment);
	let busy = $state(false);
</script>

<ResourcePage
	title="Routing schemes of {organisation}"
	description="A template can hold several models. A routing scheme decides, per call, which of them serves first and which take over when it fails: by the complexity, agent, task and tags the caller states. Assign a scheme to the organisation, a workspace or a template; the most specific one applies."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code', href: (s) => `${base}/${s.id}` },
		{ key: 'description', label: 'Description', wrap: true },
		{ key: 'rules', label: 'Rules', kind: 'number', value: (s) => s.rules.length },
		{
			key: 'default',
			label: 'Otherwise',
			value: (s) =>
				`${s.default.tiers?.length ? s.default.tiers.join(', ') : 'whole pool'} · ${ALGORITHMS.find((a) => a.value === s.default.algorithm)?.label ?? s.default.algorithm}`
		},
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'updated_at', label: 'Changed (UTC)', kind: 'datetime' }
	]}
	empty="This organisation has no routing schemes. Without one, a template with several models tries them by priority."
	deleteNote="A scheme cannot be deleted while the organisation, a workspace or a template names it."
	createHref="{base}/new"
	editHref={(s) => `${base}/${s.id}`}
	pageActions={['assign']}
>
	{#if assignment}
		<section class="card stack assignment" aria-labelledby="assignment-title">
			<h2 id="assignment-title">The organisation's scheme</h2>
			<p class="muted">
				For templates that name no scheme and whose workspace names none. Locked, it applies to
				every workspace and template of the organisation, whatever they name. Set by the
				organisation's administrators.
			</p>
			{#if assignment.writable}
				<form
					method="POST"
					action="?/assign"
					class="row"
					use:enhance={() => {
						busy = true;
						return async ({ update }) => {
							await update({ reset: false });
							busy = false;
						};
					}}
				>
					<input type="hidden" name="version" value={assignment.version} />
					<label>
						Scheme
						<select name="scheme" value={assignment.scheme ?? ''}>
							<option value="">None: the models by priority</option>
							{#each data.items as s (s.id)}
								<option value={s.name}>{s.name}{s.status === 'active' ? '' : ' (disabled)'}</option>
							{/each}
						</select>
					</label>
					<label class="inline">
						<input type="checkbox" name="locked" checked={assignment.locked} />
						Lock for every workspace and template
					</label>
					<button type="submit" class="btn btn-primary" disabled={busy}>
						{busy ? 'Saving…' : 'Save'}
					</button>
				</form>
			{:else}
				<p>
					{#if assignment.scheme}
						<code>{assignment.scheme}</code>{assignment.locked
							? ', locked: it applies to every workspace and template.'
							: '.'}
					{:else}
						None{assignment.locked ? ', locked: templates use their models by priority.' : '.'}
					{/if}
				</p>
			{/if}
			{#if assignment.own_assignments.length > 0}
				<details>
					<summary>
						{assignment.own_assignments.length} workspaces and templates name a scheme of their own{assignment.locked
							? ', which does not apply while the lock holds'
							: ''}
					</summary>
					<ul>
						{#each assignment.own_assignments as a (a)}<li>{a}</li>{/each}
					</ul>
				</details>
			{/if}
		</section>
	{/if}
</ResourcePage>

<style>
	.assignment label {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		font-size: var(--text-sm);
		font-weight: 600;
	}

	.assignment label.inline {
		flex-direction: row;
		align-items: center;
		font-weight: 400;
	}

	.assignment form {
		align-items: end;
		flex-wrap: wrap;
		gap: var(--space-3);
	}
</style>
