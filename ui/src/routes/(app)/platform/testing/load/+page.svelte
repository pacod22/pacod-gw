<script lang="ts">
	import { enhance } from '$app/forms';
	import Badge from '#lib/components/Badge.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import {
		INTERFACES,
		TARGET_TEXT,
		clock,
		loadStatusTone,
		ms,
		peakOf,
		preset,
		totalDuration,
		type LoadInterface,
		type LoadRunSummary,
		type LoadTarget,
		type Stage
	} from '#lib/load.ts';
	import { formatDuration, runDuration } from '#lib/testing.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const info = $derived(data.agents);
	// svelte-ignore state_referenced_locally
	let target = $state<LoadTarget>(data.agents?.mock_available ? 'mock' : 'external');
	let iface = $state<LoadInterface>('openai');
	let mode = $state<'managed' | 'proxy'>('managed');
	let mockFamily = $state('openai');
	let loadMode = $state<'concurrency' | 'rate'>('concurrency');
	let stages = $state<Stage[]>(preset('ramp', 20, 60));
	let presetTarget = $state(20);
	let presetSeconds = $state(60);
	let maxCalls = $state('');
	// svelte-ignore state_referenced_locally
	let chosen: string[] = $state(
		(data.agents?.agents ?? []).filter((a) => a.reachable && !a.busy).map((a) => a.name)
	);
	let busy = $state(false);

	const paid = $derived(target !== 'mock');
	const scene = $derived(target === 'mock' || target === 'live');
	const duration = $derived(totalDuration(stages));
	const peak = $derived(peakOf(stages));
	const overhead = $derived(chosen.length + (scene ? 1 : 0));
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);
	const reachable = $derived((info?.agents ?? []).filter((a) => a.reachable));
	const proxyFamily = $derived(iface === 'embeddings' ? 'openai' : iface);
	const proxyMismatch = $derived(
		target === 'mock' && mode === 'proxy' && proxyFamily !== mockFamily
	);
	const tooLong = $derived(info ? duration > info.max_duration : false);
	const canStart = $derived(
		!busy &&
			info?.configured === true &&
			!info.busy &&
			chosen.length > 0 &&
			duration > 0 &&
			peak > 0 &&
			!tooLong &&
			!proxyMismatch &&
			!(mode === 'proxy' && iface === 'native')
	);

	function usePreset(kind: 'steady' | 'ramp' | 'steps') {
		stages = preset(kind, Number(presetTarget) || 1, Number(presetSeconds) || 60);
	}

	const addStage = () => (stages = [...stages, { duration_s: 30, target: peak || 10 }]);
	const removeStage = (i: number) => (stages = stages.filter((_, k) => k !== i));

	const unit = $derived(loadMode === 'rate' ? 'calls a second' : 'calls at a time');
</script>

<PageHeader
	title="Load tests"
	description="Puts load on this gateway, or on another gateway, from load agents in containers of their own, in the stack or on other machines. The tester sets up what a test needs, adds up what the agents measure, and removes it again."
/>

{#if !data.testerConfigured || !info}
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}
{:else}
	{#if !info.configured}
		<Notice tone="info" title="No load agents are set up">
			<p>
				The tester knows no load agent. Run one (docker-compose.load.yml in the stack, or
				deploy/load-agent/ on another machine) and give the tester its address and token:
				<code>TESTER_LOAD_AGENTS</code> and <code>TESTER_LOAD_TOKEN</code>.
			</p>
		</Notice>
	{:else if info.busy}
		<Notice tone="info" title="A test is going">
			<p>
				One test runs at a time, a load test or a run of cases.
				{#if info.active}<a href="/platform/testing/load/{info.active}">Follow it</a>{:else}<a
						href="/platform/testing/runs/{info.busy}">Follow it</a
					>{/if} or wait until it ends.
			</p>
		</Notice>
	{/if}

	<form
		method="POST"
		action="?/start"
		class="stack-lg"
		use:enhance={() => {
			busy = true;
			return async ({ update }) => {
				// The form keeps what was typed, the pasted key included: it never went back to the
				// browser, it is still in the field.
				await update({ reset: false });
				busy = false;
			};
		}}
	>
		<section class="card stack">
			<h2>Target</h2>
			<div class="choices">
				{#each ['mock', 'live', 'own', 'external'] as const as t (t)}
					<label class="choice">
						<input
							type="radio"
							name="target"
							value={t}
							bind:group={target}
							disabled={t === 'mock' && !info.mock_available}
						/>
						<span>
							<strong>{TARGET_TEXT[t]}</strong>
							<span class="muted small">
								{#if t === 'mock'}
									{info.mock_available
										? 'This gateway, in front of a mock with the latency you set. No cost.'
										: 'Not available: no mock provider for load tests (TESTER_LOAD_MOCK_URL).'}
								{:else if t === 'live'}
									This gateway and a model that is set up. Real calls, at the provider's prices; at
									most {info.live_max_calls.toLocaleString('en-GB')} calls.
								{:else if t === 'own'}
									This gateway with a key of an existing workspace: its limits and routing apply.
								{:else}
									Another gateway, by its URL: a minimal gateway (pacod-gw-lite), another
									installation, or any endpoint that speaks the interface.
								{/if}
							</span>
						</span>
					</label>
				{/each}
			</div>

			{#if target === 'mock'}
				<div class="fields">
					<div class="field">
						<label for="mock-family">Provider behind the gateway</label>
						<select id="mock-family" name="mock_family" bind:value={mockFamily}>
							<option value="openai">OpenAI-compatible</option>
							<option value="anthropic">Anthropic</option>
							<option value="gemini">Gemini's own API</option>
						</select>
					</div>
					<div class="field">
						<label for="mock-delay">Delay before the answer, ms</label>
						<input
							id="mock-delay"
							name="mock_delay"
							type="number"
							min="0"
							max="60000"
							value="200"
						/>
					</div>
					<div class="field">
						<label for="mock-chunk-delay">Delay between chunks, ms</label>
						<input
							id="mock-chunk-delay"
							name="mock_chunk_delay"
							type="number"
							min="0"
							max="10000"
							value="20"
						/>
					</div>
					<div class="field">
						<label for="mock-chunks">Chunks of a streamed answer</label>
						<input
							id="mock-chunks"
							name="mock_chunks"
							type="number"
							min="1"
							max="1000"
							value="20"
						/>
					</div>
				</div>
				<p class="muted small">
					A streamed call takes at least delay + (chunks − 1) × chunk delay at the mock, a few
					delays more where the stream has other events (Anthropic's); the rest is the gateway. The
					tester writes this profile into its <code>tester-load-mock</code> providers' URLs.
				</p>
			{:else if target === 'live'}
				<div class="field">
					<label for="live-model">Model</label>
					<select id="live-model" name="live_model" required>
						<option value="">Choose a model</option>
						{#each data.models as m (m.id)}
							<option value={m.id}>{m.name} ({m.provider})</option>
						{/each}
					</select>
				</div>
			{:else}
				{#if target === 'external'}
					<div class="field">
						<label for="external-url">The gateway's URL</label>
						<input
							id="external-url"
							name="external_url"
							type="url"
							placeholder="http://10.0.0.7:14080"
							required
							autocomplete="off"
						/>
						<span class="muted small"
							>As every chosen agent reaches it. An agent in the development stack reaches a port
							published on this machine at <code>http://host.docker.internal:&lt;port&gt;</code
							>.</span
						>
					</div>
				{/if}
				<div class="fields">
					<div class="field">
						<label for="{target}-secret">Key</label>
						<input
							id="{target}-secret"
							name="{target}_secret"
							type="password"
							required
							autocomplete="off"
						/>
					</div>
					<div class="field">
						<label for="{target}-model">Model, as the gateway names it</label>
						<input
							id="{target}-model"
							name="{target}_model"
							required
							placeholder="openai/gpt-dev"
							autocomplete="off"
						/>
					</div>
				</div>
				<Notice tone="warning">
					<p>
						The key goes to every chosen agent and stays in its memory until the run ends; the
						tester keeps only a hint of it. Use a key you can revoke.
					</p>
				</Notice>
			{/if}
		</section>

		<section class="card stack">
			<h2>Request</h2>
			<div class="fields">
				<div class="field">
					<label for="interface">Interface</label>
					<select id="interface" name="interface" bind:value={iface}>
						{#each INTERFACES as i (i.id)}<option value={i.id}>{i.label}</option>{/each}
					</select>
				</div>
				<div class="field">
					<label for="mode-managed">Mode</label>
					<div class="row">
						<label class="choice"
							><input
								id="mode-managed"
								type="radio"
								name="mode"
								value="managed"
								bind:group={mode}
							/> Managed</label
						>
						<label class="choice"
							><input
								type="radio"
								name="mode"
								value="proxy"
								bind:group={mode}
								disabled={iface === 'native'}
							/> Proxy</label
						>
					</div>
				</div>
				<div class="field">
					<label for="prompt-words">Prompt, words</label>
					<input
						id="prompt-words"
						name="prompt_words"
						type="number"
						min="1"
						max="20000"
						value="60"
					/>
				</div>
				<div class="field">
					<label for="max-tokens">Answer, tokens at most</label>
					<input id="max-tokens" name="max_tokens" type="number" min="1" max="100000" value="64" />
				</div>
				<div class="field">
					<label for="http">HTTP</label>
					<select id="http" name="http">
						<option value="1.1">HTTP/1.1, one call per connection</option>
						<option value="2">HTTP/2, calls share connections</option>
					</select>
				</div>
			</div>
			<label class="choice">
				<input type="checkbox" name="stream" checked disabled={iface === 'embeddings'} />
				<span>Streamed answers <span class="muted small">– as most chat clients ask</span></span>
			</label>
			{#if proxyMismatch}
				<p class="bad small">
					In proxy mode the interface is the provider's own API: choose the matching provider above.
				</p>
			{/if}
			{#if mode === 'proxy' && (target === 'own' || target === 'external')}
				<div class="fields">
					<div class="field">
						<label for="proxy-provider">Provider, in /proxy/&lt;provider&gt;/…</label>
						<input id="proxy-provider" name="{target}_provider" required placeholder="openai" />
					</div>
					<div class="field">
						<label for="proxy-upstream">The model's id at the provider</label>
						<input id="proxy-upstream" name="{target}_upstream" required placeholder="gpt-dev" />
					</div>
					<div class="field">
						<label for="proxy-mock">Mock options (x-mock)</label>
						<input
							id="proxy-mock"
							name="{target}_mock_options"
							placeholder="delay=200,chunk_delay=20"
						/>
						<span class="muted small">When a mock provider is behind the gateway.</span>
					</div>
				</div>
			{/if}
		</section>

		<section class="card stack">
			<h2>Load</h2>
			<div class="row">
				<label class="choice"
					><input type="radio" name="load_mode" value="concurrency" bind:group={loadMode} />
					<span
						><strong>Calls at a time</strong>
						<span class="muted small"
							>Each client starts its next call when the last one has ended.</span
						></span
					></label
				>
				<label class="choice"
					><input type="radio" name="load_mode" value="rate" bind:group={loadMode} />
					<span
						><strong>Calls a second</strong>
						<span class="muted small"
							>Calls start on time whether or not earlier ones have ended.</span
						></span
					></label
				>
			</div>

			<div class="row presets">
				<span class="muted small">Start from</span>
				<button type="button" class="btn btn-quiet" onclick={() => usePreset('steady')}
					>Steady</button
				>
				<button type="button" class="btn btn-quiet" onclick={() => usePreset('ramp')}
					>Ramp up and down</button
				>
				<button type="button" class="btn btn-quiet" onclick={() => usePreset('steps')}
					>Four steps</button
				>
				<label class="small"
					>at <input class="narrow" type="number" min="1" bind:value={presetTarget} /> {unit}</label
				>
				<label class="small"
					>for <input class="narrow" type="number" min="1" bind:value={presetSeconds} /> s</label
				>
			</div>

			<table class="stages">
				<thead>
					<tr><th>Stage</th><th>Seconds</th><th>Target, {unit}</th><th></th></tr>
				</thead>
				<tbody>
					{#each stages as stage, i (i)}
						<tr>
							<td class="muted small">{i + 1}</td>
							<td
								><input
									name="stage_duration"
									type="number"
									min="0"
									bind:value={stage.duration_s}
									aria-label="Seconds of stage {i + 1}"
								/></td
							>
							<td
								><input
									name="stage_target"
									type="number"
									min="0"
									step={loadMode === 'rate' ? 'any' : '1'}
									bind:value={stage.target}
									aria-label="Target of stage {i + 1}"
								/></td
							>
							<td
								><button
									type="button"
									class="btn btn-quiet"
									disabled={stages.length === 1}
									onclick={() => removeStage(i)}>Remove</button
								></td
							>
						</tr>
					{/each}
				</tbody>
			</table>
			<div class="row">
				<button type="button" class="btn btn-quiet" onclick={addStage}>Add a stage</button>
				<span class="muted small">
					Each stage moves the target in a straight line from where the last one ended (0 at the
					start); a stage of 0 seconds jumps. Lasts {clock(duration)}, at most {peak.toLocaleString(
						'en-GB'
					)}
					{unit}.
				</span>
			</div>
			{#if tooLong}
				<p class="bad small">A load test lasts at most {clock(info.max_duration)} here.</p>
			{/if}

			<div class="fields">
				{#if loadMode === 'rate'}
					<div class="field">
						<label for="max-in-flight">Calls in flight, at most</label>
						<input id="max-in-flight" name="max_in_flight" type="number" min="1" value="500" />
						<span class="muted small"
							>Above it, a call that is due is not started, and counted.</span
						>
					</div>
				{/if}
				<div class="field">
					<label for="timeout">Timeout of a call, s</label>
					<input id="timeout" name="timeout_s" type="number" min="1" max="600" value="120" />
				</div>
				<div class="field">
					<label for="max-calls">Calls, at most{paid ? '' : ' (optional)'}</label>
					<input
						id="max-calls"
						name="max_calls"
						type="number"
						min={overhead + chosen.length}
						max={target === 'live' ? info.live_max_calls : undefined}
						required={paid}
						bind:value={maxCalls}
					/>
					<span class="muted small"
						>Every call counts, the {overhead} that check the target before the load included.</span
					>
				</div>
			</div>
		</section>

		<section class="card stack">
			<h2>Agents</h2>
			{#if info.agents.length === 0}
				<p class="muted">No agent is configured.</p>
			{:else}
				<ul class="agents">
					{#each info.agents as a (a.name)}
						<li>
							<label class="choice" class:off={!a.reachable || a.busy}>
								<input
									type="checkbox"
									name="agent"
									value={a.name}
									bind:group={chosen}
									disabled={!a.reachable || Boolean(a.busy)}
								/>
								<span>
									<strong>{a.name}</strong>
									{#if !a.reachable}
										<Badge value="not reachable" tone="bad" />
										<span class="muted small">{a.error}</span>
									{:else}
										{#if a.busy}<Badge value="busy" tone="warn" />{/if}
										<span class="muted small">
											{a.cpus} CPUs, up to {a.caps?.max_concurrency.toLocaleString('en-GB')} calls at
											a time or {a.caps?.max_rate.toLocaleString('en-GB')} a second; reaches this gateway
											at
											<code>{a.target}</code>; version {a.version}
										</span>
									{/if}
								</span>
							</label>
						</li>
					{/each}
				</ul>
				<p class="muted small">
					The load is split evenly across the chosen agents. An agent in the stack shares the
					gateway's machine: for figures of capacity, use agents on other machines.
				</p>
				{#if reachable.length === 0}
					<p class="bad small">No agent answers.</p>
				{/if}
			{/if}
		</section>

		<section class="card stack">
			<h2>Options</h2>
			<div class="field">
				<label for="label">Label</label>
				<input id="label" name="label" maxlength="80" placeholder="baseline, 2 gateways" />
			</div>
			{#if scene}
				<label class="choice">
					<input type="checkbox" name="keep_resources" />
					<span
						>Keep what the test sets up <span class="muted small"
							>– the scratch organisation stays for a look; remove it by hand afterwards</span
						></span
					>
				</label>
			{/if}
			{#if paid}
				<label class="choice">
					<input type="checkbox" name="acknowledge_cost" required />
					<span>
						The calls may cost money
						<span class="muted small"
							>– {target === 'external' || target === 'own'
								? 'the tester cannot tell what is behind the key'
								: "they reach the model's provider"}; up to {maxCalls || 'the number of'} calls</span
						>
					</span>
				</label>
			{/if}
		</section>

		{#if problem}<ProblemNotice {problem} />{/if}
		<div class="row">
			<button type="submit" class="btn btn-primary" disabled={!canStart}>Start the load test</button
			>
		</div>
	</form>

	<section class="stack">
		<h2>Recent load tests</h2>
		{#if data.runs.length === 0}
			<p class="muted">None yet.</p>
		{:else}
			<DataTable
				caption="Load tests"
				rows={data.runs}
				columns={[
					{
						key: 'created_at',
						label: 'Started (UTC)',
						kind: 'datetime',
						href: (r) => `/platform/testing/load/${r.id}`
					},
					{ key: 'label', label: 'Label', wrap: true },
					{ key: 'status', label: 'Result' },
					{ key: 'target', label: 'Target', value: (r) => TARGET_TEXT[r.target] },
					{
						key: 'calls',
						label: 'Calls',
						kind: 'number',
						value: (r) => (r.summary ? r.summary.calls.toLocaleString('en-GB') : ''),
						sortValue: (r) => r.summary?.calls ?? 0
					},
					{
						key: 'rps',
						label: 'A second',
						kind: 'number',
						value: (r) => (r.summary ? r.summary.rps_mean.toLocaleString('en-GB') : ''),
						sortValue: (r) => r.summary?.rps_mean ?? 0
					},
					{
						key: 'ttfb',
						label: 'First byte p99',
						kind: 'number',
						value: (r) => (r.summary && r.summary.ok ? ms(r.summary.ttfb.p99) : ''),
						sortValue: (r) => r.summary?.ttfb.p99 ?? 0
					},
					{
						key: 'duration',
						label: 'Took',
						kind: 'number',
						value: (r) => formatDuration(runDuration(r)),
						sortValue: (r) => runDuration(r)
					},
					{ key: 'started_by', label: 'Started by', value: (r) => r.started_by.display }
				]}
				cell={status}
				custom={['status']}
			/>
		{/if}
	</section>
{/if}

{#snippet status(run: LoadRunSummary)}
	<Badge value={run.status} tone={loadStatusTone(run.status)} />
{/snippet}

<style>
	.choices {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
		gap: var(--space-3) var(--space-5);
	}

	.fields {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(13rem, 1fr));
		gap: var(--space-3) var(--space-4);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
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

	.choice.off {
		opacity: 0.65;
	}

	.presets {
		align-items: center;
		flex-wrap: wrap;
	}

	.narrow {
		width: 6rem;
	}

	.stages {
		width: 100%;
		max-width: 36rem;
		border-collapse: collapse;
	}

	.stages th {
		padding: var(--space-1) var(--space-2);
		color: var(--text-muted);
		font-size: var(--text-sm);
		font-weight: 500;
		text-align: left;
	}

	.stages td {
		padding: var(--space-1) var(--space-2);
	}

	.stages input {
		width: 100%;
		max-width: 10rem;
	}

	.agents {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.bad {
		color: var(--danger);
	}

	h2 {
		margin: 0;
	}
</style>
