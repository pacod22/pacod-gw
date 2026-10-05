<script lang="ts">
	import { onMount } from 'svelte';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import Notice from './Notice.svelte';
	import ProblemNotice from './ProblemNotice.svelte';

	/**
	 * The result of the last form action, above the page's content: what was done, or why it
	 * was refused. A failure that belongs next to a form is shown there by the form's dialog;
	 * here it appears only when the page was rendered without one (a submission without
	 * JavaScript), or when it is not about the form's values at all.
	 */
	interface Props {
		outcome: ActionOutcome | null | undefined;
		/** Actions that have no form of their own: their failures are always shown here. */
		always?: string[];
	}

	let { outcome, always = [] }: Props = $props();

	let hydrated = $state(false);
	onMount(() => {
		hydrated = true;
	});

	const problem = $derived(
		outcome &&
			!outcome.ok &&
			outcome.problem &&
			(outcome.problem.placement === 'notice' || !hydrated || always.includes(outcome.action))
			? outcome.problem
			: null
	);
</script>

{#if outcome?.ok && outcome.message}
	<Notice tone={outcome.tone ?? 'success'}><p>{outcome.message}</p></Notice>
{/if}
{#if problem}<ProblemNotice {problem} />{/if}
