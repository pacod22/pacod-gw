<script lang="ts">
	import { formatMs } from '#lib/format.ts';
	import { OWNERS, type Stage, type Timing } from '#lib/insight/timing.ts';

	/**
	 * Where the time of one call went: a single stacked bar, one segment per timing stage in
	 * the order the stages happen, coloured by who the time belongs to. The compact form is
	 * for table rows; the full form adds a readout and the table of all stages.
	 */
	interface Props {
		timing: Timing;
		compact?: boolean;
	}

	let { timing, compact = false }: Props = $props();

	const visible = $derived(timing.stages.filter((stage) => stage.ms > 0));
	const summary = $derived(
		`Gateway delay ${formatMs(timing.gatewayDelay)} of ${formatMs(timing.total)} in total`
	);

	let active: Stage['key'] | null = $state(null);
	const current = $derived(timing.stages.find((stage) => stage.key === active) ?? null);

	function percent(share: number): string {
		if (share === 0) return '0%';
		return share < 0.001 ? '<0.1%' : `${(share * 100).toFixed(1)}%`;
	}
</script>

{#if compact}
	<div class="stagebar compact" role="img" aria-label={summary} title={summary}>
		{#if visible.length === 0}
			<span class="none"></span>
		{:else}
			{#each visible as stage (stage.key)}
				<span class="segment {stage.owner}" style:flex-grow={stage.ms}></span>
			{/each}
		{/if}
	</div>
{:else}
	<div class="stagebar full">
		{#if visible.length === 0}
			<p class="muted">No timing was recorded for this call.</p>
		{:else}
			<div class="track">
				{#each visible as stage (stage.key)}
					<button
						type="button"
						class="segment {stage.owner}"
						class:active={active === stage.key}
						style:flex-grow={stage.ms}
						aria-label="{stage.label}: {formatMs(stage.ms)}, {percent(stage.share)}"
						onpointerenter={() => (active = stage.key)}
						onpointerleave={() => (active = null)}
						onfocus={() => (active = stage.key)}
						onblur={() => (active = null)}
					></button>
				{/each}
			</div>
			<p class="readout" aria-hidden="true">
				{#if current}
					<strong>{formatMs(current.ms)}</strong>
					<span
						>{current.label} · {OWNERS[current.owner]} · {percent(current.share)} of the stages</span
					>
				{:else}
					<strong>{formatMs(timing.gatewayDelay)}</strong>
					<span
						>gateway delay, of {formatMs(timing.total)} from the first request byte to the last response
						byte</span
					>
				{/if}
			</p>
			<ul class="legend" aria-label="Colours">
				{#each Object.entries(OWNERS) as [owner, label] (owner)}
					<li><span class="swatch {owner}"></span>{label}</li>
				{/each}
			</ul>
		{/if}

		<div class="table-wrap">
			<!-- svelte-ignore a11y_no_redundant_roles (the narrow layout changes display, which drops the table semantics in Chromium and WebKit) -->
			<table role="table">
				<caption class="visually-hidden">Timing stages</caption>
				<!-- svelte-ignore a11y_no_redundant_roles -->
				<thead role="rowgroup">
					<!-- svelte-ignore a11y_no_redundant_roles -->
					<tr role="row">
						<th role="columnheader" scope="col">Stage</th>
						<th role="columnheader" scope="col">Belongs to</th>
						<th role="columnheader" scope="col" class="num">Time</th>
						<th role="columnheader" scope="col" class="num">Part of the stages</th>
					</tr>
				</thead>
				<!-- svelte-ignore a11y_no_redundant_roles -->
				<tbody role="rowgroup">
					{#each timing.stages as stage (stage.key)}
						<!-- svelte-ignore a11y_no_redundant_roles -->
						<tr
							role="row"
							class:active={active === stage.key}
							onpointerenter={() => (active = stage.key)}
							onpointerleave={() => (active = null)}
						>
							<th role="rowheader" scope="row">
								<span class="name"><span class="swatch {stage.owner}"></span>{stage.label}</span>
								<span class="help">{stage.help}</span>
							</th>
							<td role="cell">{OWNERS[stage.owner]}</td>
							<td role="cell" class="num">{formatMs(stage.ms)}</td>
							<td role="cell" class="num">{percent(stage.share)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		<p class="muted small">
			The gateway delay is the sum of the two gateway stages. In a streamed answer the stages
			overlap, so they need not add up to the total.
		</p>
	</div>
{/if}

<style>
	/* Stage colours: validated for adjacent segments in this order, light and dark. */
	.stagebar {
		--stage-client: light-dark(#eda100, #c98500);
		--stage-gateway: light-dark(#2a78d6, #3987e5);
		--stage-connection: light-dark(#eb6834, #d95926);
		--stage-provider: light-dark(#1baf7a, #199e70);
	}

	.client {
		background: var(--stage-client);
	}

	.gateway {
		background: var(--stage-gateway);
	}

	.connection {
		background: var(--stage-connection);
	}

	.provider {
		background: var(--stage-provider);
	}

	.full {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	/* Segments are separated by a gap in the surface colour, not by a border. */
	.track,
	.compact {
		display: flex;
		gap: 2px;
		width: 100%;
	}

	.track {
		height: 1.25rem;
	}

	.compact {
		min-width: 7rem;
		height: 0.5rem;
	}

	.segment {
		flex: 0 1 0;
		min-width: 3px;
		padding: 0;
		border: 0;
		border-radius: 0;
	}

	.segment:first-child {
		border-top-left-radius: 4px;
		border-bottom-left-radius: 4px;
	}

	.segment:last-child {
		border-top-right-radius: 4px;
		border-bottom-right-radius: 4px;
	}

	button.segment {
		cursor: default;
	}

	button.segment.active,
	button.segment:focus-visible {
		outline: 2px solid var(--text);
		outline-offset: 1px;
	}

	.none {
		flex: 1;
		border-radius: 4px;
		background: var(--surface-2);
	}

	.readout {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-2);
		min-height: 1.5rem;
		font-size: var(--text-sm);
		color: var(--text-muted);
	}

	.readout strong {
		color: var(--text);
		font-size: var(--text-md);
		font-variant-numeric: tabular-nums;
	}

	.legend {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2) var(--space-4);
		padding: 0;
		list-style: none;
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.legend li,
	.name {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}

	.swatch {
		flex: none;
		width: 0.75rem;
		height: 0.75rem;
		border-radius: 2px;
	}

	tbody th {
		background: none;
		color: var(--text);
		font-size: var(--text-sm);
		font-weight: 400;
		letter-spacing: 0;
		text-transform: none;
		white-space: normal;
	}

	.name {
		font-weight: 600;
	}

	.help {
		display: block;
		margin-left: calc(0.75rem + var(--space-2));
		color: var(--text-muted);
	}

	tr.active > * {
		background: var(--surface-2);
	}

	/* Narrow screens: the stage on one line, who it belongs to, its time and share below. */
	@media (max-width: 48rem) {
		table,
		thead,
		tbody {
			display: block;
		}

		thead {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
		}

		tbody tr {
			display: grid;
			grid-template-columns: minmax(0, 1fr) auto auto;
			gap: var(--space-1) var(--space-3);
			padding: var(--space-3) var(--space-4);
			border-bottom: 1px solid var(--border);
		}

		tbody tr:last-child {
			border-bottom: 0;
		}

		tbody th {
			grid-column: 1 / -1;
		}

		tbody th,
		tbody td {
			padding: 0;
			border: 0;
		}

		tbody td {
			color: var(--text-muted);
			font-size: var(--text-xs);
		}
	}
</style>
