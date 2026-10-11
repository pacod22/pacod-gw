<script lang="ts">
	import { enhance } from '$app/forms';
	import Badge from '#lib/components/Badge.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { countsText, statusTone, unavailable, type Mode } from '#lib/testing.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const catalog = $derived(data.catalog);
	// svelte-ignore state_referenced_locally
	let mode: Mode = $state(data.catalog?.mock_available ? 'mock' : 'live');
	let openai = $state('');
	let anthropic = $state('');
	let gemini = $state('');
	let busy = $state(false);

	const have = $derived(
		mode === 'mock'
			? {
					openai: true,
					anthropic: true,
					gemini: true,
					mockControl: catalog?.mock_control_available ?? false
				}
			: {
					openai: openai !== '',
					anthropic: anthropic !== '',
					gemini: gemini !== '',
					mockControl: false
				}
	);
	const total = $derived(catalog?.suites.reduce((n, s) => n + s.cases.length, 0) ?? 0);
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);

	/** Ticks or clears every case of a suite. */
	function tickSuite(event: Event, on: boolean) {
		const fieldset = (event.currentTarget as HTMLElement).closest('fieldset');
		for (const box of fieldset?.querySelectorAll<HTMLInputElement>('input[name="case"]') ?? [])
			box.checked = on;
	}
</script>

<PageHeader
	title="Testing"
	description="Runs test cases against this gateway through its public interfaces, as the signed-in platform admin. Every suite works in an organisation of its own, which is removed afterwards."
/>

{#if !data.testerConfigured || !catalog}
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}
{:else}
	{#if data.active}
		<Notice tone="info" title="A run is going">
			<p>
				Only one run goes at a time. <a href="/platform/testing/runs/{data.active}">Follow it</a>
				or wait until it ends.
			</p>
		</Notice>
	{:else if data.last}
		<p class="muted small">
			Last run: <a href="/platform/testing/runs/{data.last.id}"
				>{formatDateTime(data.last.created_at)}</a
			>
			by {data.last.started_by.display} –
			<Badge value={data.last.status} tone={statusTone(data.last.status)} />
			{countsText(data.last.counts)}
		</p>
	{/if}

	<form
		method="POST"
		action="?/start"
		class="stack-lg"
		use:enhance={() => {
			busy = true;
			return async ({ update }) => {
				await update({ reset: false });
				busy = false;
			};
		}}
	>
		<section class="card stack">
			<h2>Mode</h2>
			<div class="modes">
				<label class="choice">
					<input
						type="radio"
						name="mode"
						value="mock"
						bind:group={mode}
						disabled={!catalog.mock_available}
					/>
					<span>
						<strong>Mock</strong>
						<span class="muted small">
							{#if catalog.mock_available}
								A scripted provider answers. Exact checks, failover included, and no cost. The
								tester keeps two <code>tester-mock</code> models for it.
							{:else}
								Not available: this installation has no mock provider (TESTER_MOCK_URL).
							{/if}
						</span>
					</span>
				</label>
				<label class="choice">
					<input type="radio" name="mode" value="live" bind:group={mode} />
					<span>
						<strong>Live</strong>
						<span class="muted small">
							Models that are already set up answer. Each case makes one or two small calls to the
							real provider, at its prices.
						</span>
					</span>
				</label>
			</div>

			{#if mode === 'live'}
				<div class="models">
					<div class="field">
						<label for="openai-model">OpenAI-dialect model</label>
						<select id="openai-model" name="openai_model" bind:value={openai}>
							<option value="">None – skip the cases that need one</option>
							{#each data.models.openai as m (m.id)}
								<option value={m.id}>{m.name} ({m.provider})</option>
							{/each}
						</select>
					</div>
					<div class="field">
						<label for="anthropic-model">Anthropic model</label>
						<select id="anthropic-model" name="anthropic_model" bind:value={anthropic}>
							<option value="">None – skip the cases that need one</option>
							{#each data.models.anthropic as m (m.id)}
								<option value={m.id}>{m.name} ({m.provider})</option>
							{/each}
						</select>
					</div>
					<div class="field">
						<label for="gemini-model">Model on Gemini's own API</label>
						<select id="gemini-model" name="gemini_model" bind:value={gemini}>
							<option value="">None – skip the cases that need one</option>
							{#each data.models.gemini as m (m.id)}
								<option value={m.id}>{m.name} ({m.provider})</option>
							{/each}
						</select>
					</div>
				</div>
				{#if !openai && !anthropic && !gemini}
					<p class="muted small">Choose at least one model.</p>
				{/if}
			{/if}
		</section>

		<section class="stack">
			<h2>Cases <span class="muted small">{total} in {catalog.suites.length} suites</span></h2>
			{#each catalog.suites as suite (suite.suite)}
				<fieldset class="stack">
					<legend>
						{suite.title}
						{#if suite.source === 'uploaded'}<Badge value="uploaded" tone="neutral" />{/if}
					</legend>
					<div class="suite-head">
						{#if suite.description}<p class="muted small">{suite.description}</p>{/if}
						<div class="row">
							<button type="button" class="btn btn-quiet" onclick={(e) => tickSuite(e, true)}
								>All</button
							>
							<button type="button" class="btn btn-quiet" onclick={(e) => tickSuite(e, false)}
								>None</button
							>
						</div>
					</div>
					<ul class="cases">
						{#each suite.cases as c (c.id)}
							{@const why = unavailable(c, mode, have)}
							<li>
								<label class="choice" class:off={why}>
									<input type="checkbox" name="case" value={c.id} checked />
									<span>
										{c.title}
										<span class="muted small mono">{c.id}</span>
										{#if why}<span class="muted small">– skipped: {why}</span>{/if}
										{#if c.description}<span class="muted small block">{c.description}</span>{/if}
									</span>
								</label>
							</li>
						{/each}
					</ul>
				</fieldset>
			{/each}
		</section>

		<section class="card stack">
			<h2>Options</h2>
			<label class="choice">
				<input type="checkbox" name="stop_on_fail" />
				<span
					>Stop after the first failure <span class="muted small">– the rest is skipped</span></span
				>
			</label>
			<label class="choice">
				<input type="checkbox" name="keep_resources" />
				<span>
					Keep what the run creates
					<span class="muted small"
						>– the scratch organisations stay for a look; remove them by hand afterwards</span
					>
				</span>
			</label>
		</section>

		{#if problem}<ProblemNotice {problem} />{/if}
		<div class="row">
			<button
				type="submit"
				class="btn btn-primary"
				disabled={busy ||
					data.active !== null ||
					(mode === 'live' && !openai && !anthropic && !gemini)}>Start the run</button
			>
		</div>
	</form>
{/if}

<style>
	.modes,
	.models {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
		gap: var(--space-3) var(--space-5);
	}

	.choice {
		display: flex;
		align-items: flex-start;
		gap: var(--space-2);
		font-weight: 400;
	}

	.choice input {
		margin-top: 0.2rem;
	}

	.choice > span {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.cases .choice > span {
		display: block;
	}

	.choice.off {
		opacity: 0.65;
	}

	.block {
		display: block;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	.suite-head {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--space-2);
	}

	.suite-head p {
		flex: 1 1 20rem;
	}

	.cases {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin: 0;
		padding: 0;
		list-style: none;
	}

	h2 .small {
		font-weight: 400;
	}
</style>
