<script lang="ts">
	import { tick } from 'svelte';
	import Check from '@lucide/svelte/icons/check';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import Search from '@lucide/svelte/icons/search';
	import { filterChoices, type ScopeChoice } from '#lib/nav.ts';

	/**
	 * Picks one organisation or workspace from a list that can be long: a button that shows
	 * the current choice and opens a list with a search box. Arrow keys move, Enter picks,
	 * Escape closes.
	 */
	interface Props {
		id: string;
		label: string;
		choices: ScopeChoice[];
		/** The chosen id; empty for none. */
		value: string;
		/** Shown when nothing is chosen. */
		placeholder: string;
		/** An entry above the list that clears the choice, for example "No workspace". */
		noneLabel?: string;
		disabled?: boolean;
		onpick: (id: string) => void;
	}

	let {
		id,
		label,
		choices,
		value,
		placeholder,
		noneLabel,
		disabled = false,
		onpick
	}: Props = $props();

	let open = $state(false);
	let query = $state('');
	let active = $state(0);
	let root: HTMLDivElement | undefined = $state();
	let search: HTMLInputElement | undefined = $state();
	let button: HTMLButtonElement | undefined = $state();

	const current = $derived(choices.find((c) => c.id === value));
	const shown = $derived(filterChoices(choices, query));
	/** The entries of the list: "none" first when it is offered and nothing is searched for. */
	const entries = $derived([
		...(noneLabel && value && !query ? [{ id: '', name: '', label: noneLabel }] : []),
		...shown
	]);

	async function show() {
		if (disabled) return;
		open = true;
		query = '';
		active = Math.max(
			0,
			entries.findIndex((e) => e.id === value)
		);
		await tick();
		search?.focus();
	}

	function close(refocus = true) {
		open = false;
		if (refocus) button?.focus();
	}

	function pick(choice: { id: string }) {
		close(false);
		if (choice.id !== value) onpick(choice.id);
		else button?.focus();
	}

	async function keydown(event: KeyboardEvent) {
		if (event.key === 'ArrowDown') {
			event.preventDefault();
			active = Math.min(entries.length - 1, active + 1);
		} else if (event.key === 'ArrowUp') {
			event.preventDefault();
			active = Math.max(0, active - 1);
		} else if (event.key === 'Enter') {
			event.preventDefault();
			if (entries[active]) pick(entries[active]);
		} else if (event.key === 'Escape') {
			event.preventDefault();
			close();
		} else {
			return;
		}
		await tick();
		root?.querySelector(`#${id}-option-${active}`)?.scrollIntoView({ block: 'nearest' });
	}

	function outside(event: PointerEvent) {
		if (open && root && !root.contains(event.target as Node)) close(false);
	}
</script>

<svelte:window onpointerdown={outside} />

<div class="combobox" bind:this={root}>
	<span class="label" id="{id}-label">{label}</span>
	<button
		{id}
		bind:this={button}
		type="button"
		class="trigger"
		aria-haspopup="listbox"
		aria-expanded={open}
		aria-labelledby="{id}-label {id}"
		{disabled}
		onclick={() => (open ? close() : show())}
	>
		<span class="text" class:muted={!current}>
			{current?.label ?? (value ? 'Unknown' : placeholder)}
		</span>
		<ChevronsUpDown size={14} aria-hidden="true" />
	</button>

	{#if open}
		<div class="popover">
			<div class="search">
				<Search size={14} aria-hidden="true" />
				<input
					bind:this={search}
					bind:value={query}
					type="text"
					role="combobox"
					aria-label="Search {label.toLowerCase()}"
					aria-controls="{id}-list"
					aria-expanded="true"
					aria-autocomplete="list"
					aria-activedescendant={entries.length > 0 ? `${id}-option-${active}` : undefined}
					placeholder="Search…"
					autocomplete="off"
					spellcheck="false"
					oninput={() => (active = 0)}
					onkeydown={keydown}
				/>
			</div>
			<ul id="{id}-list" role="listbox" aria-labelledby="{id}-label">
				{#each entries as entry, index (entry.id)}
					<li
						id="{id}-option-{index}"
						role="option"
						aria-selected={entry.id === value}
						class:active={index === active}
						class:none={entry.id === ''}
						onpointerenter={() => (active = index)}
						onclick={() => pick(entry)}
						onkeydown={() => {}}
					>
						<span class="check" aria-hidden="true">
							{#if entry.id === value}<Check size={14} />{/if}
						</span>
						<span class="entry">
							<span>{entry.label}</span>
							{#if entry.name && entry.name !== entry.label}
								<span class="slug">{entry.name}</span>
							{/if}
						</span>
						{#if 'status' in entry && entry.status === 'disabled'}
							<span class="off">disabled</span>
						{/if}
					</li>
				{:else}
					<li class="empty" role="presentation">Nothing matches “{query}”.</li>
				{/each}
			</ul>
		</div>
	{/if}
</div>

<style>
	.combobox {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.label {
		color: var(--sidebar-muted, var(--text-muted));
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}

	.trigger {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-2);
		width: 100%;
		min-height: var(--control-height);
		padding: var(--space-1) var(--space-3);
		border: 1px solid var(--border-strong);
		border-radius: var(--radius);
		background: var(--surface);
		color: var(--text);
		font-size: var(--text-sm);
		text-align: left;
		cursor: pointer;
	}

	.trigger:focus-visible {
		outline: none;
		border-color: var(--accent);
		box-shadow: var(--focus-ring);
	}

	.trigger:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}

	.text {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.popover {
		position: absolute;
		top: 100%;
		right: 0;
		left: 0;
		z-index: 30;
		margin-top: var(--space-1);
		border: 1px solid var(--border-strong);
		border-radius: var(--radius);
		background: var(--surface);
		box-shadow: var(--shadow);
	}

	.search {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		padding: 0 var(--space-3);
		border-bottom: 1px solid var(--border);
		color: var(--text-muted);
	}

	.search input {
		width: 100%;
		min-height: 2.25rem;
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--text);
		font-size: var(--text-sm);
	}

	.search input:focus-visible {
		box-shadow: none;
	}

	ul {
		max-height: 18rem;
		overflow-y: auto;
		margin: 0;
		padding: var(--space-1);
		list-style: none;
	}

	li {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		padding: var(--space-2);
		border-radius: var(--radius-sm);
		color: var(--text);
		font-size: var(--text-sm);
		cursor: pointer;
	}

	li.active {
		background: var(--surface-2);
	}

	li.none {
		color: var(--text-muted);
	}

	.check {
		display: inline-flex;
		flex: none;
		width: 14px;
		color: var(--accent);
	}

	.entry {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
	}

	.slug {
		color: var(--text-faint);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
	}

	.off {
		color: var(--text-faint);
		font-size: 0.6875rem;
	}

	.empty {
		color: var(--text-muted);
		cursor: default;
	}
</style>
