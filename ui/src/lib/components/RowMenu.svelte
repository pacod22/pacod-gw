<script lang="ts">
	import type { Snippet } from 'svelte';
	import Ellipsis from '@lucide/svelte/icons/ellipsis';

	/**
	 * The actions of one table row behind a single button. The list opens in the top layer
	 * (the popover API), so the table's scrolling container cannot clip it. Escape and a click
	 * elsewhere close it, and so does scrolling, which would leave it behind.
	 */
	interface Props {
		/** Names the button, for example "Actions for openai". */
		label: string;
		children: Snippet;
	}

	let { label, children }: Props = $props();
	const uid = $props.id();
	let button: HTMLButtonElement | undefined = $state();
	let panel: HTMLDivElement | undefined = $state();
	let open = $state(false);

	const GAP = 4;
	const MARGIN = 8;

	/**
	 * Before the list is shown: below the button, its right edge on the button's. Set here,
	 * in "beforetoggle", so that the list never appears elsewhere first; the "toggle" event
	 * comes a moment after the list is painted.
	 */
	function anchor(event: ToggleEvent) {
		if (event.newState !== 'open' || !button || !panel) return;
		const box = button.getBoundingClientRect();
		panel.style.left = 'auto';
		panel.style.right = `${Math.max(MARGIN, window.innerWidth - box.right)}px`;
		panel.style.top = `${box.bottom + GAP}px`;
	}

	/** Once shown, with its size known: above the button when there is no room below. */
	function fit() {
		if (!button || !panel) return;
		const box = button.getBoundingClientRect();
		const menu = panel.getBoundingClientRect();
		const above = box.top - GAP - menu.height;
		if (menu.bottom > window.innerHeight - MARGIN && above >= MARGIN)
			panel.style.top = `${above}px`;
		if (menu.left < MARGIN) {
			panel.style.left = `${MARGIN}px`;
			panel.style.right = 'auto';
		}
	}

	function close() {
		if (panel?.matches(':popover-open')) panel.hidePopover();
	}

	function ontoggle(event: ToggleEvent) {
		open = event.newState === 'open';
		if (open) {
			fit();
			panel?.querySelector<HTMLElement>('a[href], button:not(:disabled)')?.focus();
			window.addEventListener('scroll', close, { capture: true, passive: true });
			window.addEventListener('resize', close);
		} else {
			window.removeEventListener('scroll', close, { capture: true });
			window.removeEventListener('resize', close);
		}
	}

	/**
	 * An item was used: close the list and give focus back to the button, before an item
	 * opens a dialog. A dialog returns focus to what had it when it opened; a hidden item
	 * cannot take it back.
	 */
	function used(event: Event) {
		const target = event.target as Element | null;
		if (event.type === 'click' && !target?.closest('a[href], button')) return;
		if (event.type === 'click' && target?.closest('button[type="submit"]')) return;
		close();
		button?.focus();
	}

	function onkeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') {
			event.preventDefault();
			close();
			button?.focus();
			return;
		}
		if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
		const items = [
			...(panel?.querySelectorAll<HTMLElement>('a[href], button:not(:disabled)') ?? [])
		];
		const at = items.indexOf(document.activeElement as HTMLElement);
		const next = event.key === 'ArrowDown' ? at + 1 : at - 1;
		items[(next + items.length) % items.length]?.focus();
		event.preventDefault();
	}
</script>

<button
	bind:this={button}
	type="button"
	class="trigger"
	popovertarget="{uid}-menu"
	aria-expanded={open}
	aria-controls="{uid}-menu"
	aria-label={label}
	title={label}
>
	<Ellipsis size={18} aria-hidden="true" />
</button>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions (the handlers serve the items inside: arrow keys move between them, and using one closes the list) -->
<div
	bind:this={panel}
	id="{uid}-menu"
	class="menu"
	popover="auto"
	onbeforetoggle={anchor}
	{ontoggle}
	{onkeydown}
	onclick={used}
	onsubmit={used}
	role="group"
	aria-label={label}
>
	{@render children()}
</div>

<style>
	.trigger {
		display: inline-grid;
		place-items: center;
		width: 2rem;
		height: 2rem;
		padding: 0;
		border: 1px solid transparent;
		border-radius: 0.5rem;
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		transition:
			background-color 0.15s,
			border-color 0.15s,
			color 0.15s;
	}

	.trigger:hover,
	.trigger[aria-expanded='true'] {
		border-color: var(--border);
		background: var(--surface-2);
		color: var(--text);
	}

	.menu {
		position: fixed;
		inset: auto;
		min-width: 12rem;
		max-width: min(20rem, calc(100vw - 1rem));
		margin: 0;
		padding: 0.375rem;
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		background: var(--surface);
		color: var(--text);
		box-shadow: var(--shadow);
	}

	.menu:popover-open {
		display: flex;
		flex-direction: column;
		gap: 1px;
	}

	/* What the pages put in: links, buttons and one-button forms, shown as menu items. */
	.menu :global(form) {
		display: contents;
	}

	.menu :global(.btn) {
		justify-content: flex-start;
		width: 100%;
		min-height: 2.125rem;
		padding: 0 0.625rem;
		border: 0;
		border-radius: 0.5rem;
		background: transparent;
		color: var(--text);
		font-family: var(--font);
		font-size: var(--text-sm);
		font-weight: 500;
		letter-spacing: 0;
		text-align: left;
	}

	.menu :global(.btn:hover),
	.menu :global(.btn:focus-visible) {
		background: var(--surface-2);
		color: var(--text);
		text-decoration: none;
	}

	.menu :global(.btn.danger) {
		color: var(--danger);
	}

	.menu :global(.btn.danger:hover),
	.menu :global(.btn.danger:focus-visible) {
		background: var(--danger-soft);
		color: var(--danger);
	}

	.menu :global(.btn:focus-visible) {
		outline-offset: -2px;
		border-radius: 0.5rem;
	}
</style>
