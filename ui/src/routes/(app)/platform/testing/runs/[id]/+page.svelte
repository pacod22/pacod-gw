<script lang="ts">
	import { onMount } from 'svelte';
	import { enhance } from '$app/forms';
	import { invalidate } from '$app/navigation';
	import Badge from '#lib/components/Badge.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import {
		countsText,
		doneCount,
		formatDuration,
		isFinished,
		runDuration,
		statusTone,
		type StepResult
	} from '#lib/testing.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const run = $derived(data.run);
	const finished = $derived(run ? isFinished(run.status) : true);
	const leftovers = $derived(run?.suites.flatMap((s) => s.leftovers) ?? []);
	let now = $state(Date.now());

	// While the run goes, its events reload the page's data; one reload answers a burst.
	onMount(() => {
		if (!data.run || isFinished(data.run.status)) return;
		const source = new EventSource(`/platform/testing/runs/${data.run.id}/events`);
		let timer: ReturnType<typeof setTimeout> | undefined;
		const reload = () => {
			clearTimeout(timer);
			timer = setTimeout(() => void invalidate('app:testing-run'), 250);
		};
		source.addEventListener('progress', reload);
		source.addEventListener('end', () => {
			source.close();
			clearTimeout(timer);
			void invalidate('app:testing-run');
		});
		source.addEventListener('error', () => {
			if (source.readyState === EventSource.CLOSED) reload();
		});
		const tick = setInterval(() => (now = Date.now()), 1000);
		return () => {
			source.close();
			clearTimeout(timer);
			clearInterval(tick);
		};
	});

	/** The steps a case shows: every step of a failed case, the failed ones otherwise. */
	const open = (status: string) => status === 'failed' || status === 'error';
</script>

{#if !run}
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}
{:else}
	<PageHeader
		title="Run {run.id}"
		crumbs={[{ label: 'Testing', href: '/platform/testing/history' }]}
		description="{run.mode === 'mock' ? 'Mock' : 'Live'} mode, started by {run.started_by.display}."
	>
		{#snippet actions()}
			{#if !finished}
				<form method="POST" action="?/cancel" use:enhance>
					<button type="submit" class="btn btn-danger">Cancel the run</button>
				</form>
			{:else}
				<a class="btn" href="/platform/testing">New run</a>
			{/if}
		{/snippet}
	</PageHeader>

	<OutcomeNotice outcome={form} always={['cancel']} />
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}

	<section class="card stack">
		<div class="row summary">
			<Badge value={run.status} tone={statusTone(run.status)} />
			{#if finished}
				<span>{countsText(run.counts)} of {run.counts.total}</span>
			{:else}
				<span>{doneCount(run.counts)} of {run.counts.total} done</span>
				<span class="muted small live">updating as the run goes</span>
			{/if}
		</div>
		<dl class="facts">
			<dt>Started (UTC)</dt>
			<dd>{run.started_at ? formatDateTime(run.started_at, true) : 'Waiting'}</dd>
			<dt>Took</dt>
			<dd>{formatDuration(runDuration(run, now))}</dd>
			{#if run.mode === 'live'}
				<dt>Models</dt>
				<dd class="mono small">
					{[run.params.openai_model, run.params.anthropic_model, run.params.gemini_model]
						.filter(Boolean)
						.join(', ')}
				</dd>
			{/if}
			<dt>Options</dt>
			<dd>
				{[
					run.stop_on_fail ? 'stop after the first failure' : '',
					run.keep_resources ? 'keep what the run creates' : ''
				]
					.filter(Boolean)
					.join(', ') || 'None'}
			</dd>
		</dl>
		{#if run.error}
			<Notice tone="error" title="The run ended early"><p>{run.error}</p></Notice>
		{/if}
		{#if leftovers.length > 0}
			<Notice
				tone="warning"
				title={run.keep_resources ? 'Kept for a look' : 'Not everything could be removed'}
			>
				<p>Remove these by hand when they are no longer needed:</p>
				<ul class="mono small">
					{#each leftovers as path (path)}<li>{path}</li>{/each}
				</ul>
			</Notice>
		{/if}
	</section>

	{#each run.suites as suite (suite.suite)}
		<section class="stack suite">
			<h2>
				{suite.title}
				{#if suite.status === 'running'}<Badge value="running" tone="warn" />{/if}
			</h2>

			{#if suite.setup.length > 0}
				<details class="phase" open={suite.setup.some((s) => open(s.status))}>
					<summary>
						Setup: {suite.setup.length} steps
						{#if suite.setup.some((s) => open(s.status))}<Badge value="failed" tone="bad" />{/if}
					</summary>
					{@render steps(suite.setup)}
				</details>
			{/if}

			<ol class="cases">
				{#each suite.cases as c (c.id)}
					<li class="case">
						<details open={open(c.status)}>
							<summary>
								<Badge value={c.status} tone={statusTone(c.status)} />
								<span class="title">{c.title}</span>
								{#if c.duration_ms}<span class="muted small">{formatDuration(c.duration_ms)}</span
									>{/if}
							</summary>
							<!-- A failed step shows its own failures; the reason is for cases without steps. -->
							{#if c.reason && c.steps.length === 0}
								<p class="reason small" class:bad={open(c.status)}>{c.reason}</p>
							{/if}
							{#if c.steps.length > 0}{@render steps(c.steps)}{/if}
						</details>
					</li>
				{/each}
			</ol>

			{#if suite.teardown.length > 0}
				<details class="phase" open={suite.leftovers.length > 0}>
					<summary>
						Clean-up: {suite.teardown.length - suite.leftovers.length} of {suite.teardown.length}
						removed
					</summary>
					{@render steps(suite.teardown)}
				</details>
			{/if}
		</section>
	{/each}
{/if}

{#snippet steps(list: StepResult[])}
	<ol class="steps">
		{#each list as step, i (i)}
			<li>
				<div class="step-head">
					<Badge value={step.status} tone={statusTone(step.status)} />
					<span>{step.name}</span>
					{#if step.detail}<span class="muted small">{step.detail}</span>{/if}
				</div>
				{#if step.failures.length > 0}
					<ul class="failures small">
						{#each step.failures as failure, j (j)}<li>{failure}</li>{/each}
					</ul>
				{/if}
				{#if step.request || step.response}
					<details class="exchange">
						<summary class="small">Request and response</summary>
						{#if step.request}
							<p class="small mono">{step.request.method} {step.request.url}</p>
							{#if Object.keys(step.request.headers).length > 0}
								<pre>{Object.entries(step.request.headers)
										.map(([k, v]) => `${k}: ${v}`)
										.join('\n')}</pre>
							{/if}
							{#if step.request.body}<pre>{step.request.body}</pre>{/if}
						{/if}
						{#if step.response}
							<p class="small mono">
								→ {step.response.status} in {formatDuration(step.response.elapsed_ms)}
							</p>
							{#if Object.keys(step.response.headers).length > 0}
								<pre>{Object.entries(step.response.headers)
										.map(([k, v]) => `${k}: ${v}`)
										.join('\n')}</pre>
							{/if}
							{#if step.response.body}<pre>{step.response.body}</pre>{/if}
						{/if}
					</details>
				{/if}
			</li>
		{/each}
	</ol>
{/snippet}

<style>
	.summary {
		align-items: center;
		gap: var(--space-3);
	}

	.live::before {
		display: inline-block;
		width: 0.5rem;
		height: 0.5rem;
		margin-right: var(--space-1);
		border-radius: 50%;
		background: var(--warning);
		content: '';
	}

	.suite {
		margin-top: var(--space-4);
	}

	.cases,
	.steps {
		display: flex;
		flex-direction: column;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.cases {
		gap: var(--space-2);
	}

	.case > details {
		padding: var(--space-2) var(--space-3);
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		background: var(--surface);
	}

	summary {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
		cursor: pointer;
	}

	.title {
		font-weight: 550;
	}

	.reason {
		margin: var(--space-2) 0 0;
		color: var(--text-muted);
		overflow-wrap: anywhere;
	}

	.reason.bad,
	.failures {
		color: var(--danger);
	}

	.phase > summary {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.steps {
		gap: var(--space-2);
		margin-top: var(--space-2);
		padding-left: var(--space-3);
		border-left: 2px solid var(--border);
	}

	.step-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
		overflow-wrap: anywhere;
	}

	.failures {
		margin: var(--space-1) 0 0;
		padding-left: var(--space-5);
		overflow-wrap: anywhere;
	}

	.exchange {
		margin-top: var(--space-1);
	}

	.exchange > summary {
		color: var(--text-muted);
	}

	.exchange p {
		margin: var(--space-2) 0 var(--space-1);
		overflow-wrap: anywhere;
	}

	pre {
		max-height: 18rem;
		margin: 0 0 var(--space-1);
		padding: var(--space-2) var(--space-3);
		overflow: auto;
		border-radius: var(--radius);
		background: var(--surface-2);
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	ul {
		padding-left: var(--space-5);
	}
</style>
