<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import ActionDialog from './ActionDialog.svelte';

	/** Asks before a destructive action. The button names the action; Cancel is the way out. */
	interface Props {
		open: boolean;
		title: string;
		/** The form action that does it, such as `?/delete`. */
		action: string;
		/** The label of the button: "Delete provider". */
		confirm: string;
		outcome: ActionOutcome | null | undefined;
		/** Sent along: the id of the object, its name for the message. */
		hidden?: Record<string, string | number | undefined>;
		/** What will happen, in a sentence or two. */
		children: Snippet;
		onclose?: () => void;
	}

	let {
		open = $bindable(false),
		title,
		action,
		confirm,
		outcome,
		hidden,
		children,
		onclose
	}: Props = $props();
</script>

<ActionDialog bind:open {title} {action} submit={confirm} {outcome} {hidden} {onclose} danger>
	<div class="stack">{@render children()}</div>
</ActionDialog>
