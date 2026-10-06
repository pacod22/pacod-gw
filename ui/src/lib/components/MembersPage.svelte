<script lang="ts">
	import { untrack } from 'svelte';
	import { enhance } from '$app/forms';
	import { page } from '$app/state';
	import { roleLabel } from '#lib/access.ts';
	import { formatDateTime } from '#lib/format.ts';
	import {
		describeMemberRole,
		ORG_ROLES,
		ROLE_HELP,
		WORKSPACE_ROLES,
		type Invitation,
		type InviteResult,
		type Member
	} from '#lib/members.ts';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import type { MembersData } from '#lib/server/members.ts';
	import ActionDialog from './ActionDialog.svelte';
	import Badge from './Badge.svelte';
	import ConfirmDialog from './ConfirmDialog.svelte';
	import CopyButton from './CopyButton.svelte';
	import DataTable from './DataTable.svelte';
	import Dialog from './Dialog.svelte';
	import OutcomeNotice from './OutcomeNotice.svelte';
	import PageHeader from './PageHeader.svelte';
	import ProblemNotice from './ProblemNotice.svelte';

	/**
	 * Who works in an organisation or a workspace, with their roles, the open invitations,
	 * and, for organisation admins, the dialogs that invite people and change their access.
	 */
	interface Props {
		title: string;
		description: string;
		data: MembersData;
		form: ActionOutcome | null | undefined;
		/** On a workspace's page: the workspace. */
		workspace?: { id: string; name: string };
	}

	let { title, description, data, form, workspace }: Props = $props();

	interface Row {
		id: string;
		member: Member;
		/** Roles that apply here and can be changed here. */
		own: Member['roles'];
		/** On a workspace's page: roles that come from the organisation. */
		inherited: Member['roles'];
		/** On an organisation's page: roles in its workspaces. */
		workspaces: Member['roles'];
	}

	const rows = $derived(
		data.members.map((m): Row => ({
			id: m.principal_id,
			member: m,
			own: m.roles.filter((r) =>
				workspace ? r.scope.type === 'workspace' && !r.inherited : r.scope.type === 'organisation'
			),
			inherited: m.roles.filter((r) => r.inherited),
			workspaces: workspace ? [] : m.roles.filter((r) => r.scope.type === 'workspace')
		}))
	);
	const pending = $derived(data.invitations?.filter((i) => !i.expired) ?? []);
	const expired = $derived(data.invitations?.filter((i) => i.expired) ?? []);

	// --- inviting -------------------------------------------------------------------------
	let inviting = $state(untrack(() => page.url.searchParams.has('invite') && data.canManage));
	let where = $state(untrack(() => workspace?.id ?? ''));
	let inviteRole = $state('viewer');
	const inviteRoles = $derived(where ? WORKSPACE_ROLES : ORG_ROLES);
	$effect(() => {
		// A role that does not fit the place falls back to viewer.
		if (!inviteRoles.includes(inviteRole as never)) untrack(() => (inviteRole = 'viewer'));
	});

	// The links of invitations that were not emailed: shown once, after inviting or resending.
	let resultsOpen = $state(false);
	let results: InviteResult[] = $state([]);
	$effect(() => {
		const next =
			form?.ok && (form.action === 'invite' || form.action === 'resend')
				? (form.data as { results?: InviteResult[] } | undefined)?.results
				: undefined;
		if (next && next.some((r) => r.link || !r.ok || r.message)) {
			untrack(() => {
				results = next;
				resultsOpen = true;
			});
		}
	});

	// --- changing access --------------------------------------------------------------------
	let editing: Row | null = $state(null);
	let editOpen = $state(false);
	let removing: Row | null = $state(null);
	let removeOpen = $state(false);
	let revoking: Invitation | null = $state(null);
	let revokeOpen = $state(false);

	/** The member's role in a scope, `''` for none: the first one when there are several. */
	function roleIn(member: Member, type: 'organisation' | 'workspace', id: string): string {
		return (
			member.roles.find(
				(r) => r.scope.type === type && 'id' in r.scope && r.scope.id === id && !r.inherited
			)?.role ?? ''
		);
	}

	const orgId = $derived(page.params.org ?? '');
	const accessScopes = $derived(
		workspace
			? [
					{
						key: workspace.id,
						label: workspace.name,
						type: 'workspace' as const,
						roles: WORKSPACE_ROLES
					}
				]
			: [
					{
						key: 'org',
						label: 'The organisation',
						type: 'organisation' as const,
						roles: ORG_ROLES
					},
					...data.workspaces.map((w) => ({
						key: w.id,
						label: w.name,
						type: 'workspace' as const,
						roles: WORKSPACE_ROLES
					}))
				]
	);
	const placeOf = (i: Invitation) =>
		i.workspace_name ? `workspace ${i.workspace_name}` : 'the organisation';
</script>

<div class="stack-lg">
	<PageHeader {title} {description}>
		{#snippet actions()}
			{#if data.canManage}
				<button type="button" class="btn btn-primary" onclick={() => (inviting = true)}>
					Invite people
				</button>
			{/if}
		{/snippet}
	</PageHeader>

	<OutcomeNotice outcome={form} />

	{#if !data.canManage}
		<p class="muted">
			Organisation admins invite people and change their roles. You can see who works here.
		</p>
	{/if}

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if rows.length === 0}
		<div class="card empty">
			<p>Nobody has a role here yet.</p>
			{#if data.canManage}
				<button type="button" class="btn btn-primary" onclick={() => (inviting = true)}>
					Invite people
				</button>
			{/if}
		</div>
	{:else}
		<DataTable
			caption={title}
			{rows}
			filter={rows.length > 8}
			columns={[
				{
					key: 'person',
					label: 'Person',
					value: (r) => `${r.member.name} ${r.member.email ?? ''}`,
					sortValue: (r) => r.member.name.toLowerCase()
				},
				{
					key: 'role',
					label: workspace ? 'Role here' : 'Organisation role',
					value: (r) => [...r.own, ...r.inherited].map(describeMemberRole).join(', ')
				},
				...(workspace
					? []
					: [
							{
								key: 'workspaces',
								label: 'Workspaces',
								value: (r: Row) => r.workspaces.map(describeMemberRole).join(', ')
							}
						]),
				{ key: 'status', label: 'Status', value: (r) => r.member.status }
			]}
			custom={['person', 'role', 'workspaces', 'status']}
			actions={data.canManage ? rowActions : undefined}
			menu={(r) => `Actions for ${r.member.name}`}
		>
			{#snippet cell(r, column)}
				{#if column.key === 'person'}
					<span class="person">
						<span>{r.member.name}</span>
						{#if r.member.email && r.member.email !== r.member.name}
							<span class="muted small">{r.member.email}</span>
						{/if}
						{#if r.member.kind === 'token'}<span class="muted small">Admin token</span>{/if}
					</span>
				{:else if column.key === 'role'}
					<span class="badges">
						{#each r.own as role (role.binding_id)}
							<Badge value={roleLabel(role.role)} tone="good" />
						{/each}
						{#each r.inherited as role (role.binding_id)}
							<span class="inherited">
								<Badge value={roleLabel(role.role)} tone="neutral" />
								<span class="muted small">via organisation</span>
							</span>
						{/each}
						{#if r.own.length === 0 && r.inherited.length === 0}<span class="muted">–</span>{/if}
					</span>
				{:else if column.key === 'workspaces'}
					{#if r.workspaces.length === 0}
						<span class="muted">–</span>
					{:else}
						<ul class="ws-roles">
							{#each r.workspaces as role (role.binding_id)}<li>
									{describeMemberRole(role)}
								</li>{/each}
						</ul>
					{/if}
				{:else if column.key === 'status'}
					<Badge value={r.member.status} />
				{/if}
			{/snippet}
		</DataTable>
	{/if}

	{#if data.invitations !== null}
		<section class="stack" aria-labelledby="invitations-title">
			<h2 id="invitations-title">Open invitations</h2>
			{#if data.invitations.length === 0}
				<p class="muted">No open invitations.</p>
			{:else}
				<DataTable
					caption="Open invitations"
					rows={[...pending, ...expired]}
					columns={[
						{ key: 'email', label: 'Email' },
						{
							key: 'role',
							label: 'Invited as',
							value: (i) => `${roleLabel(i.role)} of ${placeOf(i)}`
						},
						{ key: 'invited_by', label: 'Invited by' },
						{
							key: 'expires_at',
							label: 'Valid until (UTC)',
							value: (i) => (i.expired ? 'Expired' : formatDateTime(i.expires_at))
						}
					]}
					actions={invitationActions}
					menu={(i) => `Actions for the invitation of ${i.email}`}
				/>
			{/if}
		</section>
	{/if}
</div>

{#snippet rowActions(r: Row)}
	{#if r.own.length > 0 || !workspace}
		<button
			type="button"
			class="btn btn-quiet"
			onclick={() => {
				editing = r;
				editOpen = true;
			}}
		>
			{workspace ? 'Change role' : 'Edit access'}<span class="visually-hidden">
				of {r.member.name}</span
			>
		</button>
	{/if}
	{#if r.own.length > 0 || (!workspace && r.workspaces.length > 0)}
		<button
			type="button"
			class="btn btn-quiet danger"
			onclick={() => {
				removing = r;
				removeOpen = true;
			}}
		>
			Remove<span class="visually-hidden"> {r.member.name}</span>
		</button>
	{/if}
{/snippet}

{#snippet invitationActions(i: Invitation)}
	<form method="POST" action="?/resend" use:enhance>
		<input type="hidden" name="id" value={i.id} />
		<button type="submit" class="btn btn-quiet">
			{i.expired ? 'Renew' : 'Resend'}<span class="visually-hidden">
				the invitation of {i.email}</span
			>
		</button>
	</form>
	<button
		type="button"
		class="btn btn-quiet danger"
		onclick={() => {
			revoking = i;
			revokeOpen = true;
		}}
	>
		Revoke<span class="visually-hidden"> the invitation of {i.email}</span>
	</button>
{/snippet}

{#if data.canManage}
	<ActionDialog
		bind:open={inviting}
		title="Invite people"
		action="?/invite"
		submit="Send invitations"
		outcome={form}
	>
		<div class="field">
			<label for="invite-emails">Email addresses</label>
			<textarea
				id="invite-emails"
				name="emails"
				rows="3"
				required
				placeholder="ann@example.com, ben@example.com"
				aria-describedby="invite-emails-help"></textarea>
			<p class="help" id="invite-emails-help">
				One or more, separated by commas or lines. Everyone gets their own link; whoever has no
				account yet chooses a password when accepting.
			</p>
		</div>
		{#if workspace}
			<input type="hidden" name="where" value={workspace.id} />
		{:else}
			<div class="field">
				<label for="invite-where">Access to</label>
				<select id="invite-where" name="where" bind:value={where}>
					<option value="">The whole organisation</option>
					{#each data.workspaces as w (w.id)}
						<option value={w.id}>Workspace {w.name}</option>
					{/each}
				</select>
			</div>
		{/if}
		<fieldset class="roles">
			<legend>Role</legend>
			{#each inviteRoles as role (role)}
				<label class="role">
					<input type="radio" name="role" value={role} bind:group={inviteRole} />
					<span>
						<strong>{roleLabel(role)}</strong>
						<span class="muted small">{ROLE_HELP[role]}</span>
					</span>
				</label>
			{/each}
		</fieldset>
	</ActionDialog>

	<Dialog bind:open={resultsOpen} title="Invitations">
		<div class="stack">
			<ul class="results">
				{#each results as result (result.email)}
					<li class="stack-sm">
						<strong>{result.email}</strong>
						{#if !result.ok}
							<span class="error-text">Not invited: {result.message}</span>
						{:else if result.emailed}
							<span class="muted small">The invitation was emailed.</span>
						{:else if result.link}
							<span class="muted small">
								{result.message
									? `The email could not be sent (${result.message}). Send this link yourself:`
									: 'This installation sends no email. Send this link yourself:'}
							</span>
							<span class="link-row">
								<input
									class="mono"
									type="text"
									readonly
									value={result.link}
									aria-label="Invitation link for {result.email}"
									onfocus={(e) => e.currentTarget.select()}
								/>
								<CopyButton
									value={result.link}
									class="btn btn-primary"
									what="the link for {result.email}"
								/>
							</span>
						{/if}
					</li>
				{/each}
			</ul>
			<p class="muted small">
				A link works once and only until the invitation expires. It cannot be shown again; “Resend”
				makes a new one.
			</p>
			<div class="buttons">
				<button type="button" class="btn" onclick={() => (resultsOpen = false)}>Done</button>
			</div>
		</div>
	</Dialog>

	{#if editing}
		{@const member = editing.member}
		<ActionDialog
			bind:open={editOpen}
			title="{workspace ? 'Role' : 'Access'} of {member.name}"
			action="?/access"
			submit="Save"
			outcome={form}
			hidden={{ principal: member.principal_id, name: member.name }}
		>
			<p class="muted small">
				{workspace
					? 'Their role in this workspace. Roles from the organisation apply here as well.'
					: 'One role in the organisation and one in each workspace, or none. An organisation role applies to all its workspaces.'}
			</p>
			<div class="access">
				{#each accessScopes as scope (scope.key)}
					{@const current =
						scope.key === 'org'
							? roleIn(member, 'organisation', orgId)
							: roleIn(member, 'workspace', scope.key)}
					<label for="access-{scope.key}">{scope.label}</label>
					<input type="hidden" name="was:{scope.key}" value={current} />
					<select id="access-{scope.key}" name="role:{scope.key}" value={current}>
						<option value="">No role</option>
						{#each scope.roles as role (role)}
							<option value={role}>{roleLabel(role)}</option>
						{/each}
					</select>
				{/each}
			</div>
		</ActionDialog>
	{/if}

	{#if removing}
		<ConfirmDialog
			bind:open={removeOpen}
			title="Remove {removing.member.name}?"
			action="?/remove"
			confirm="Remove"
			outcome={form}
			hidden={{ principal: removing.member.principal_id, name: removing.member.name }}
		>
			<p>
				{workspace
					? `${removing.member.name} loses their roles in this workspace at once. Roles they have in the organisation stay.`
					: `${removing.member.name} loses every role in the organisation and its workspaces at once. Their account stays.`}
			</p>
		</ConfirmDialog>
	{/if}

	{#if revoking}
		<ConfirmDialog
			bind:open={revokeOpen}
			title="Revoke the invitation of {revoking.email}?"
			action="?/revoke"
			confirm="Revoke"
			outcome={form}
			hidden={{ id: revoking.id, email: revoking.email }}
		>
			<p>The link stops working at once. You can invite the address again later.</p>
		</ConfirmDialog>
	{/if}
{/if}

<style>
	.person,
	.stack-sm {
		display: flex;
		flex-direction: column;
		gap: 0.125rem;
	}

	.badges {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.inherited {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
	}

	.ws-roles {
		padding: 0;
		list-style: none;
	}

	.empty {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--space-3);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.help {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.roles {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		padding: 0;
		border: 0;
	}

	.roles legend {
		margin-bottom: var(--space-2);
		font-weight: 600;
	}

	.role {
		display: flex;
		align-items: flex-start;
		gap: var(--space-2);
		padding: var(--space-2) var(--space-3);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		cursor: pointer;
	}

	.role:has(input:checked) {
		border-color: var(--accent);
		background: var(--accent-soft);
	}

	.role span {
		display: flex;
		flex-direction: column;
	}

	.access {
		display: grid;
		grid-template-columns: minmax(8rem, auto) 1fr;
		align-items: center;
		gap: var(--space-2) var(--space-3);
	}

	.results {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		padding: 0;
		list-style: none;
	}

	.link-row {
		display: flex;
		gap: var(--space-2);
	}

	.link-row input {
		flex: 1;
		min-width: 0;
	}

	.error-text {
		color: var(--danger);
	}

	.buttons {
		display: flex;
		justify-content: flex-end;
	}
</style>
