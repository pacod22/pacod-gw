<script lang="ts">
	import { untrack } from 'svelte';
	import { enhance } from '$app/forms';
	import { replaceState } from '$app/navigation';
	import { page } from '$app/state';
	import Badge from '#lib/components/Badge.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import RolesEditor from '#lib/components/RolesEditor.svelte';
	import SchemaForm from '#lib/components/SchemaForm.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	// What the form shows: the user, the fields and the roles as the page opened or was last
	// saved. A live update of the data does not overwrite what someone is typing.
	const snapshot = () => ({
		user: data.user,
		fields: data.fields,
		roles: data.roles,
		scopes: data.scopes
	});
	let view = $state.raw(untrack(snapshot));
	/** Sent back with a save: the core refuses it when someone else changed the user since. */
	let version = $state(untrack(() => data.user?.version));
	let generation = $state(0);
	function refresh() {
		view = snapshot();
		version = data.user?.version;
		generation += 1;
	}
	let shownId = untrack(() => data.user?.id);
	$effect(() => {
		const id = data.user?.id;
		if (id !== shownId) {
			shownId = id;
			untrack(refresh);
		}
	});

	let busy = $state(false);
	let rolesReady = $state(true);
	let deleteOpen = $state(false);

	const user = $derived(data.user);
	const name = $derived(view.user?.email ?? 'the new user');
	const saveProblem = $derived(
		form && !form.ok && form.action === 'save' && form.problem?.placement === 'form'
			? form.problem
			: null
	);

	// Said once after creating the user, which ends on this page by a redirect.
	let created: { rolesFailed: string } | null = $state(null);
	// The router keeps the address it navigated to: say it once per user, not after each action.
	let announced = '';
	$effect(() => {
		const params = page.url.searchParams;
		const id = data.user?.id ?? '';
		if (!params.has('created') || id === announced) return;
		announced = id;
		untrack(() => {
			created = { rolesFailed: params.get('roles_failed') ?? '' };
			replaceState(page.url.pathname, page.state);
		});
	});
	$effect(() => {
		if (form) untrack(() => (created = null));
	});
</script>

<div class="stack-lg">
	<PageHeader
		title={data.isNew ? 'New user' : name}
		description={data.isNew
			? 'An account that signs in to this console. Give it a role, or it can do nothing.'
			: 'The account, its roles and what can be done with it.'}
		crumbs={[{ label: 'Users', href: '/platform/access/users' }]}
	/>

	{#if created && !created.rolesFailed}
		<Notice tone="success"><p>The user {name} was created.</p></Notice>
	{:else if created}
		<Notice tone="warning">
			<p>
				The user {name} was created, but not every role was given: {created.rolesFailed} Check the roles
				below and save again.
			</p>
		</Notice>
	{/if}
	<OutcomeNotice outcome={form} always={['verification', 'reset', 'confirm']} />

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else}
		<div class="layout" class:single={data.isNew}>
			<form
				method="POST"
				action="?/save"
				class="card stack-lg"
				use:enhance={() => {
					busy = true;
					return async ({ result, update }) => {
						// Keep what was typed when the save failed.
						await update({ reset: false });
						busy = false;
						if (result.type === 'success') refresh();
					};
				}}
			>
				{#if version !== undefined}<input type="hidden" name="version" value={version} />{/if}
				{#if saveProblem}<ProblemNotice problem={saveProblem} />{/if}

				<fieldset class="bare stack" disabled={!data.canManage}>
					<legend><h2>Account</h2></legend>
					{#key generation}
						<SchemaForm
							fields={view.fields}
							object={view.user ?? undefined}
							mode={data.isNew ? 'create' : 'replace'}
							problem={saveProblem}
							idPrefix="user"
						/>
					{/key}
				</fieldset>

				<fieldset class="bare stack" disabled={!data.canManage}>
					<legend><h2>Roles</h2></legend>
					{#if view.roles === null}
						{#if data.rolesProblem}<ProblemNotice problem={data.rolesProblem} />{/if}
					{:else}
						{#key generation}
							<RolesEditor
								name={data.isNew ? 'the user' : name}
								roles={view.roles}
								scopes={view.scopes}
								bind:ready={rolesReady}
							/>
						{/key}
					{/if}
				</fieldset>

				{#if data.canManage}
					<div class="buttons">
						<a class="btn" href="/platform/access/users">{data.isNew ? 'Cancel' : 'Back'}</a>
						<button type="submit" class="btn btn-primary" disabled={busy || !rolesReady}>
							{busy ? 'Saving…' : data.isNew ? 'Create user' : 'Save changes'}
						</button>
					</div>
				{/if}
			</form>

			{#if user}
				<aside class="stack-lg">
					<section class="card stack" aria-labelledby="details-title">
						<h2 id="details-title">Details</h2>
						<dl class="facts">
							<dt>Status</dt>
							<dd><Badge value={user.status} /></dd>
							<dt>Address</dt>
							<dd>{user.email_verified ? 'Confirmed' : 'Not confirmed'}</dd>
							<dt>Created</dt>
							<dd>{formatDateTime(user.created_at)} UTC</dd>
							<dt>Changed</dt>
							<dd>{formatDateTime(user.updated_at)} UTC</dd>
							<dt>Id</dt>
							<dd class="id">
								<span class="mono small">{user.id}</span>
								<CopyButton value={user.id} label="Copy id" what="of {user.email}" />
							</dd>
						</dl>
					</section>

					{#if data.canManage}
						<section class="card stack" aria-labelledby="signin-title">
							<h2 id="signin-title">Signing in</h2>
							{#if !user.email_verified}
								<p class="muted small">
									The address is not confirmed yet, so {user.email} cannot sign in. Send the link again,
									or mark the address confirmed when email does not arrive.
								</p>
								<div class="actions">
									{@render action('verification', 'Send verification email')}
									{@render action('confirm', 'Mark address confirmed')}
								</div>
							{/if}
							{#if user.status === 'active'}
								<p class="muted small">
									A password-reset email lets the user choose a new password. To set one yourself,
									type it under Account and save.
								</p>
								<div class="actions">{@render action('reset', 'Send password reset')}</div>
							{:else}
								<p class="muted small">The account is not active: the user cannot sign in.</p>
							{/if}
						</section>

						<section class="card stack" aria-labelledby="delete-title">
							<h2 id="delete-title">Delete user</h2>
							<p class="muted small">
								The user is signed out everywhere and their roles are taken away. This cannot be
								undone.
							</p>
							<div class="actions">
								<button type="button" class="btn btn-danger" onclick={() => (deleteOpen = true)}>
									Delete user
								</button>
							</div>
						</section>
					{/if}
				</aside>
			{/if}
		</div>
	{/if}
</div>

{#snippet action(name: string, label: string)}
	<form
		method="POST"
		action="?/{name}"
		use:enhance={() =>
			async ({ result, update }) => {
				await update();
				// The account changed on the server: a later save must send its new version.
				if (result.type === 'success') version = data.user?.version;
			}}
	>
		<input type="hidden" name="email" value={user?.email} />
		<button type="submit" class="btn">{label}</button>
	</form>
{/snippet}

{#if user && data.canManage}
	<ConfirmDialog
		bind:open={deleteOpen}
		title="Delete {user.email}?"
		action="?/delete"
		confirm="Delete user"
		outcome={form}
		hidden={{ email: user.email }}
	>
		<p>
			{user.email} is signed out everywhere and loses every role at once. This cannot be undone.
		</p>
	</ConfirmDialog>
{/if}

<style>
	.layout {
		display: grid;
		grid-template-columns: minmax(0, 44rem) minmax(16rem, 24rem);
		align-items: start;
		gap: var(--space-5);
	}

	.layout.single {
		grid-template-columns: minmax(0, 44rem);
	}

	@media (max-width: 64rem) {
		.layout {
			grid-template-columns: minmax(0, 1fr);
		}
	}

	.bare {
		min-width: 0;
		padding: 0;
		border: 0;
	}

	.bare legend {
		padding: 0;
		margin-bottom: var(--space-3);
	}

	.buttons {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: var(--space-2);
	}

	.facts {
		display: grid;
		grid-template-columns: auto 1fr;
		align-items: baseline;
		gap: var(--space-2) var(--space-3);
		margin: 0;
	}

	.facts dt {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.facts dd {
		min-width: 0;
		margin: 0;
	}

	.id {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--space-1);
		overflow-wrap: anywhere;
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}
</style>
