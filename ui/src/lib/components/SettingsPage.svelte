<script lang="ts" generics="T extends { name: string; status?: string; version?: number }">
	import type { Snippet } from 'svelte';
	import { enhance } from '$app/forms';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import type { SettingsData } from '#lib/server/settings.ts';
	import Badge from './Badge.svelte';
	import ConfirmDialog from './ConfirmDialog.svelte';
	import OutcomeNotice from './OutcomeNotice.svelte';
	import PageHeader from './PageHeader.svelte';
	import ProblemNotice from './ProblemNotice.svelte';
	import SchemaForm from './SchemaForm.svelte';

	/**
	 * The settings of an organisation or a workspace: one card per section, each saved on its
	 * own, and a danger zone at the end. What the caller may only read is shown disabled.
	 */
	interface Inside {
		label: string;
		/** `null` when it could not be counted. */
		count: number | null;
		more?: boolean;
		href: string;
	}

	interface Props {
		title: string;
		description?: string;
		crumbs?: { label: string; href: string }[];
		data: SettingsData<T>;
		form: ActionOutcome | null | undefined;
		/** What the object still holds: it can be deleted only when all of it is gone. */
		inside: Inside[];
		/** What disabling it does, in a sentence. */
		disableNote: string;
		/** Under fields the caller may not change while they may change others next to them. */
		lockedNote?: string;
		children?: Snippet;
	}

	let {
		title,
		description,
		crumbs,
		data,
		form,
		inside,
		disableNote,
		lockedNote = 'You cannot change these.',
		children
	}: Props = $props();

	let busy: string | null = $state(null);
	let disabling = $state(false);
	let deleting = $state(false);
	let typed = $state('');

	const noun = $derived(data.noun);
	const item = $derived(data.item);
	const version = $derived(typeof item?.version === 'number' ? item.version : undefined);
	const problemOf = (key: string) =>
		form && !form.ok && form.action === 'save' && form.id === key ? (form.problem ?? null) : null;
	const statusProblem = $derived(
		form && !form.ok && form.action === 'status' ? (form.problem ?? null) : null
	);
	const filled = $derived(inside.filter((i) => i.count === null || i.count > 0));
	const empty = $derived(filled.length === 0);
	const active = $derived(item?.status !== 'disabled');
	/** "A workspace", "An organisation". */
	const article = $derived(/^[aeiou]/i.test(data.noun) ? 'An' : 'A');
</script>

<div class="stack-lg">
	<PageHeader {title} {description} {crumbs} />
	<OutcomeNotice outcome={form} />
	{@render children?.()}

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if item}
		{#each data.sections as section (section.key)}
			{@const problem = problemOf(section.key)}
			<section class="card stack section" id={section.key} aria-labelledby="{section.key}-title">
				<header class="stack-sm">
					<h2 id="{section.key}-title">{section.title}</h2>
					{#if section.description}<p class="muted small">{section.description}</p>{/if}
				</header>
				{#if section.fields.length > 0}
					<form
						method="POST"
						action="?/save"
						class="stack"
						use:enhance={() => {
							busy = section.key;
							return async ({ update }) => {
								// Keep what was typed: after a save it is what the object now holds.
								await update({ reset: false });
								busy = null;
							};
						}}
					>
						<input type="hidden" name="section" value={section.key} />
						{#if version !== undefined}<input type="hidden" name="version" value={version} />{/if}
						{#if problem}<ProblemNotice {problem} />{/if}
						<SchemaForm
							fields={section.fields}
							object={item}
							mode="replace"
							{problem}
							idPrefix={section.key}
						/>
						<div class="row">
							<button type="submit" class="btn btn-primary" disabled={busy !== null}>
								{busy === section.key ? 'Saving…' : `Save ${section.title.toLowerCase()}`}
							</button>
						</div>
					</form>
				{/if}
				{#if section.readFields.length > 0}
					<fieldset class="bare stack" disabled>
						<legend class="visually-hidden">{section.title}, read only</legend>
						<SchemaForm
							fields={section.readFields}
							object={item}
							mode="replace"
							idPrefix="{section.key}-read"
						/>
					</fieldset>
					<p class="muted small">
						{section.fields.length > 0
							? lockedNote
							: 'Your role lets you see these values but not change them.'}
					</p>
				{/if}
			</section>
		{/each}

		<section class="card stack danger-zone" id="danger" aria-labelledby="danger-title">
			<header class="stack-sm">
				<h2 id="danger-title">Danger zone</h2>
			</header>

			<div class="zone-row">
				<div class="stack-sm">
					<h3>Status <Badge value={item.status ?? 'active'} /></h3>
					<p class="muted small">{disableNote}</p>
					{#if statusProblem}<ProblemNotice problem={statusProblem} />{/if}
				</div>
				{#if data.canSetStatus}
					{#if active}
						<button type="button" class="btn btn-danger-outline" onclick={() => (disabling = true)}>
							Disable {noun}
						</button>
					{:else}
						<form method="POST" action="?/status" use:enhance>
							<input type="hidden" name="status" value="active" />
							{#if version !== undefined}<input type="hidden" name="version" value={version} />{/if}
							<button type="submit" class="btn">Enable {noun}</button>
						</form>
					{/if}
				{/if}
			</div>

			<div class="zone-row">
				<div class="stack-sm">
					<h3>Delete this {noun}</h3>
					{#if empty}
						<p class="muted small">
							The {noun} is empty and can be deleted. Its members lose their roles here and open invitations
							are withdrawn. This cannot be undone.
						</p>
					{:else}
						<p class="muted small">
							{article}
							{noun} can be deleted only when nothing is left in it. Remove these first, or disable the
							{noun} instead:
						</p>
						<ul class="inside">
							{#each filled as entry (entry.label)}
								<li>
									<a href={entry.href}>{entry.label}</a>
									<span class="muted">
										{entry.count === null
											? 'cannot be counted'
											: `${entry.count}${entry.more ? '+' : ''}`}
									</span>
								</li>
							{/each}
						</ul>
					{/if}
				</div>
				{#if data.canDelete}
					<button
						type="button"
						class="btn btn-danger"
						disabled={!empty}
						onclick={() => {
							typed = '';
							deleting = true;
						}}
					>
						Delete {noun}
					</button>
				{/if}
			</div>
		</section>

		<ConfirmDialog
			bind:open={disabling}
			title="Disable {noun} “{item.name}”?"
			action="?/status"
			confirm="Disable {noun}"
			outcome={form}
			hidden={{ status: 'disabled', version }}
		>
			<p>{disableNote}</p>
			<p>You can enable it again at any time.</p>
		</ConfirmDialog>

		<ConfirmDialog
			bind:open={deleting}
			title="Delete {noun} “{item.name}”?"
			action="?/delete"
			confirm="Delete {noun}"
			outcome={form}
			ready={typed.trim() === item.name}
		>
			<p>This cannot be undone. Type the name of the {noun} to confirm.</p>
			<label class="stack-sm">
				<span>Name</span>
				<input
					name="confirm"
					class="mono"
					autocomplete="off"
					spellcheck="false"
					placeholder={item.name}
					bind:value={typed}
				/>
			</label>
		</ConfirmDialog>
	{/if}
</div>

<style>
	.section,
	.danger-zone {
		max-width: 48rem;
	}

	.stack-sm {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	h2 {
		font-size: var(--text-lg);
	}

	h3 {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		font-size: var(--text-md);
	}

	.danger-zone {
		border-color: var(--danger);
	}

	.zone-row {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--space-3);
		padding-top: var(--space-3);
		border-top: 1px solid var(--border);
	}

	.zone-row > .stack-sm {
		flex: 1 1 18rem;
	}

	.inside {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		padding-left: var(--space-5);
	}

	.inside li {
		display: flex;
		gap: var(--space-2);
	}

	.bare {
		padding: 0;
		border: 0;
	}

	.btn-danger-outline {
		border-color: var(--danger);
		color: var(--danger);
	}

	.btn-danger-outline:hover {
		background: var(--danger-soft);
	}
</style>
