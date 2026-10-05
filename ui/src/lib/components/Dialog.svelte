<script lang="ts">
	import type { Snippet } from 'svelte';

	/** A modal dialog on the native `<dialog>`: focus is kept inside and Escape closes it. */
	interface Props {
		open: boolean;
		title: string;
		children: Snippet;
		onclose?: () => void;
	}

	let { open = $bindable(false), title, children, onclose }: Props = $props();
	const uid = $props.id();
	let element: HTMLDialogElement | undefined = $state();

	$effect(() => {
		if (!element) return;
		if (open && !element.open) {
			const dialog = element;
			dialog.showModal();
			// Start at the first control of the content rather than at Close. For a
			// confirmation that is Cancel, the safe choice. Waits for the content to render.
			requestAnimationFrame(() => {
				dialog
					.querySelector<HTMLElement>(
						'.body input:not([type="hidden"]), .body select, .body textarea, .body button'
					)
					?.focus();
			});
		} else if (!open && element.open) {
			element.close();
		}
	});

	function closed() {
		open = false;
		onclose?.();
	}
</script>

<dialog bind:this={element} aria-labelledby="{uid}-title" onclose={closed}>
	<header>
		<h2 id="{uid}-title">{title}</h2>
		<button type="button" class="btn btn-quiet" onclick={() => (open = false)}>Close</button>
	</header>
	<div class="body">
		{@render children()}
	</div>
</dialog>

<style>
	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
		padding: var(--space-3) var(--space-4);
		border-bottom: 1px solid var(--border);
	}

	h2 {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.body {
		padding: var(--space-4);
	}
</style>
