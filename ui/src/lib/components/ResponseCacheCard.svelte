<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import { formatDateTime } from '#lib/format.ts';
	import { cacheSettingsText, cacheSourceLabel } from '#lib/plugins.ts';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import Badge from './Badge.svelte';
	import ConfirmDialog from './ConfirmDialog.svelte';
	import Notice from './Notice.svelte';

	/**
	 * The response cache of an organisation or a workspace (docs/plugin-concept.md, section 15):
	 * the binding that answers its calls, when it was last cleared, and a button to clear it.
	 */
	interface Props {
		cache: Schemas['ScopeCacheView'];
		scope: 'organisation' | 'workspace';
		canClear: boolean;
		/** The Insight page of the scope's figures. */
		statsHref: string;
		outcome: ActionOutcome | null | undefined;
	}

	let { cache, scope, canClear, statsHref, outcome }: Props = $props();

	let clearOpen = $state(false);
	const binding = $derived(cache.binding);
	const cleared = $derived(cache.cleared);
	// Which of the clears that reach the scope is the one in force.
	const lastClear = $derived(
		[
			{ by: 'this ' + scope, clear: cleared.own },
			{ by: 'its organisation', clear: cleared.organisation },
			{ by: 'the platform', clear: cleared.platform }
		]
			.filter((c) => c.clear)
			.sort((a, b) => Date.parse(b.clear!.at) - Date.parse(a.clear!.at))[0]
	);
	const what = $derived(
		scope === 'workspace' ? 'this workspace' : 'every workspace of this organisation'
	);
</script>

<section class="card stack" aria-labelledby="response-cache">
	<div class="head">
		<h2 id="response-cache">Response cache</h2>
		<a href={statsHref}>Its figures in Insight</a>
	</div>

	{#if binding}
		<dl class="facts">
			<dt>Answers calls</dt>
			<dd>
				{#if binding.mode === 'enforce'}
					<Badge value="on" tone="good" />
				{:else}
					<Badge value="shadow" tone="neutral" />
					<span class="muted">records what it would have answered; providers answer every call</span
					>
				{/if}
			</dd>
			<dt>From</dt>
			<dd>
				{cacheSourceLabel(binding.source)}{binding.locked ? ', locked for its workspaces' : ''}
			</dd>
			<dt>Keeps</dt>
			<dd>{cacheSettingsText(binding.settings)}</dd>
			{#if binding.timeout_ms}
				<dt>Waits for the store</dt>
				<dd>at most {binding.timeout_ms} ms</dd>
			{/if}
		</dl>
	{:else}
		<p>No response cache answers calls here: {cache.why_none ?? 'no binding'}.</p>
	{/if}
	{#if cache.problem}
		<Notice tone="warning" title="The binding cannot answer calls">
			<p>{cache.problem}</p>
		</Notice>
	{/if}

	<p class="muted">
		{#if lastClear?.clear}
			Last cleared {formatDateTime(lastClear.clear.at)} by {lastClear.by}{lastClear.clear.by_display
				? ` (${lastClear.clear.by_display})`
				: ''}: answers kept before then are not used.
		{:else}
			Never cleared. Kept answers expire after their time.
		{/if}
	</p>

	{#if canClear}
		<div>
			<button type="button" class="btn" onclick={() => (clearOpen = true)}>Clear the cache</button>
		</div>
		<ConfirmDialog
			bind:open={clearOpen}
			title="Clear the response cache?"
			action="?/clearCache"
			confirm="Clear the cache"
			{outcome}
		>
			<p>
				The answers kept for {what} are no longer used. Its next calls are answered by their providers,
				and cost what they cost, until their answers are kept again.
			</p>
		</ConfirmDialog>
	{/if}
</section>

<style>
	.head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--space-3);
		flex-wrap: wrap;
	}
</style>
