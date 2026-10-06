<script lang="ts">
	import {
		ALGORITHMS,
		COMPLEXITY_MATCHES,
		emptyRule,
		failoverDraft,
		schemeDraft,
		schemeOf,
		type ActionDraft,
		type FailoverDraft,
		type RoutingScheme
	} from '#lib/routing.ts';

	/**
	 * The controls of a routing scheme. The scheme travels in one hidden field, `scheme`, as
	 * JSON: its rules are a list that grows and shrinks on the page. The surrounding `<form>`
	 * and its buttons belong to the page.
	 */
	interface Props {
		scheme?: Partial<RoutingScheme> | null;
		disabled?: boolean;
	}

	let { scheme = null, disabled = false }: Props = $props();

	// The editor starts from the scheme it was given and then owns its state.
	// svelte-ignore state_referenced_locally
	let draft = $state(schemeDraft(scheme));
	let asJson = $state(false);
	let jsonText = $state('');
	let jsonError = $state('');

	const value = $derived(asJson ? jsonText : JSON.stringify(schemeOf(draft)));

	function toggleJson() {
		if (!asJson) {
			jsonText = JSON.stringify(schemeOf(draft), null, 2);
			jsonError = '';
			asJson = true;
			return;
		}
		try {
			draft = schemeDraft(JSON.parse(jsonText));
			asJson = false;
		} catch (e) {
			jsonError = `This is not valid JSON: ${(e as Error).message}`;
		}
	}

	function move(i: number, by: number) {
		const j = i + by;
		if (j < 0 || j >= draft.rules.length) return;
		const rules = [...draft.rules];
		[rules[i], rules[j]] = [rules[j], rules[i]];
		draft.rules = rules;
	}

	const algorithmHelp = (a: string) => ALGORITHMS.find((x) => x.value === a)?.help ?? '';
</script>

{#snippet failoverControls(f: FailoverDraft, id: string)}
	<div class="grid">
		<label for="{id}-max">Models to try, at most</label>
		<input id="{id}-max" type="number" min="1" max="8" bind:value={f.max_targets} {disabled} />
		<label for="{id}-attempts">Attempts on each model</label>
		<input
			id="{id}-attempts"
			type="number"
			min="1"
			max="5"
			placeholder="1, or the template's for a single model"
			bind:value={f.attempts_per_target}
			{disabled}
		/>
		<label for="{id}-per">Time per model (ms)</label>
		<input
			id="{id}-per"
			type="number"
			min="100"
			placeholder="The template's timeout"
			bind:value={f.per_target_timeout_ms}
			{disabled}
		/>
		<label for="{id}-deadline">Stop trying after (ms)</label>
		<input
			id="{id}-deadline"
			type="number"
			min="100"
			placeholder="No limit"
			bind:value={f.deadline_ms}
			{disabled}
		/>
	</div>
	<div class="checks" role="group" aria-label="Move on to the next model after">
		<span class="label">Move on after</span>
		<label><input type="checkbox" bind:checked={f.rate_limited} {disabled} /> 429 rate limit</label>
		<label><input type="checkbox" bind:checked={f.server_error} {disabled} /> 5xx errors</label>
		<label><input type="checkbox" bind:checked={f.connect_error} {disabled} /> no connection</label>
		<label
			><input type="checkbox" bind:checked={f.limit_refused} {disabled} /> a limit of this model</label
		>
		<label><input type="checkbox" bind:checked={f.timeout} {disabled} /> a timeout</label>
	</div>
	<p class="help">
		A call moves on only before any of the answer reached the caller. A timeout is off by default:
		the provider may be generating, and billing, the answer already.
	</p>
{/snippet}

{#snippet actionControls(a: ActionDraft, id: string)}
	<div class="grid">
		<label for="{id}-tiers">Tiers</label>
		<input
			id="{id}-tiers"
			placeholder="Empty: the whole pool. In order: small, large"
			bind:value={a.tiers}
			autocomplete="off"
			{disabled}
		/>
		<label for="{id}-algorithm">First model by</label>
		<select id="{id}-algorithm" bind:value={a.algorithm} {disabled}>
			{#each ALGORITHMS as option (option.value)}<option value={option.value}>{option.label}</option
				>{/each}
		</select>
		<span></span>
		<p class="help">{algorithmHelp(a.algorithm)}</p>
		<label for="{id}-affinity">Sessions</label>
		<select id="{id}-affinity" bind:value={a.affinity} {disabled}>
			<option value="none">Each call on its own</option>
			<option value="session">Calls of a session stay on one model</option>
		</select>
		<label for="{id}-overflow">When the tiers fail</label>
		<select id="{id}-overflow" bind:value={a.overflow} {disabled}>
			<option value="none">The call fails</option>
			<option value="rest_of_pool">Try the rest of the pool</option>
		</select>
	</div>
	<label class="inline">
		<input
			type="checkbox"
			checked={a.failover !== null}
			onchange={(e) => (a.failover = e.currentTarget.checked ? failoverDraft() : null)}
			{disabled}
		/>
		A failover policy of its own
	</label>
	{#if a.failover}{@render failoverControls(a.failover, `${id}-fo`)}{/if}
{/snippet}

<input type="hidden" name="scheme" {value} />

<div class="row end">
	<button type="button" class="btn btn-quiet" onclick={toggleJson} {disabled}>
		{asJson ? 'Back to the form' : 'Edit as JSON'}
	</button>
</div>

{#if asJson}
	<fieldset class="section" {disabled}>
		<legend>Scheme as JSON</legend>
		<textarea
			class="json"
			rows="24"
			bind:value={jsonText}
			spellcheck="false"
			aria-label="The scheme as JSON"></textarea>
		{#if jsonError}<p class="error">{jsonError}</p>{/if}
	</fieldset>
{:else}
	<fieldset class="section" {disabled}>
		<legend>Scheme</legend>
		<div class="grid">
			<label for="rs-name">Name</label>
			<input id="rs-name" bind:value={draft.name} required autocomplete="off" />
			<span></span>
			<p class="help">
				Slug, unique in the organisation. Templates, workspaces and the organisation name it.
			</p>
			<label for="rs-description">Description</label>
			<input id="rs-description" bind:value={draft.description} autocomplete="off" />
			<label for="rs-status">Status</label>
			<select id="rs-status" bind:value={draft.status}>
				<option value="active">Active</option>
				<option value="disabled">Disabled: the next level's scheme applies</option>
			</select>
		</div>
	</fieldset>

	<fieldset class="section" {disabled}>
		<legend>Rules</legend>
		<p class="help">
			Tried in order; the first whose conditions all hold decides. A text condition is empty (any
			value), <code>*</code> (must be given), <code>-</code> (must not be given) or a list:
			<code>review, ci-*</code> matches <code>review</code> and anything starting with
			<code>ci-</code>.
		</p>
		{#each draft.rules as rule, i (i)}
			<article class="rule" aria-label="Rule {rule.name}">
				<header class="row">
					<strong>{i + 1}.</strong>
					<input aria-label="Rule name" class="name" bind:value={rule.name} autocomplete="off" />
					<span class="grow"></span>
					<button
						type="button"
						class="btn btn-quiet"
						onclick={() => move(i, -1)}
						disabled={disabled || i === 0}>Up</button
					>
					<button
						type="button"
						class="btn btn-quiet"
						onclick={() => move(i, 1)}
						disabled={disabled || i === draft.rules.length - 1}>Down</button
					>
					<button
						type="button"
						class="btn btn-danger"
						onclick={() => (draft.rules = draft.rules.filter((_, k) => k !== i))}
						{disabled}>Remove</button
					>
				</header>
				<div class="checks" role="group" aria-label="Complexity">
					<span class="label">Complexity</span>
					{#each COMPLEXITY_MATCHES as c (c)}
						<label>
							<input type="checkbox" value={c} bind:group={rule.complexity} />
							{c === 'unset' ? 'not stated' : c}
						</label>
					{/each}
				</div>
				<div class="grid">
					<label for="r{i}-agent">Agent</label>
					<input id="r{i}-agent" bind:value={rule.agent_id} autocomplete="off" />
					<label for="r{i}-task">Task</label>
					<input id="r{i}-task" bind:value={rule.task} autocomplete="off" />
					<label for="r{i}-user">User</label>
					<input id="r{i}-user" bind:value={rule.user_id} autocomplete="off" />
					<label for="r{i}-template">Template</label>
					<input id="r{i}-template" bind:value={rule.template} autocomplete="off" />
					<label for="r{i}-tags">Tags</label>
					<textarea id="r{i}-tags" rows="2" placeholder="team = search, ads" bind:value={rule.tags}
					></textarea>
					<label for="r{i}-min">Input tokens, at least</label>
					<input id="r{i}-min" type="number" min="0" placeholder="0" bind:value={rule.at_least} />
					<label for="r{i}-max">Input tokens, below</label>
					<input
						id="r{i}-max"
						type="number"
						min="1"
						placeholder="no limit"
						bind:value={rule.below}
					/>
				</div>
				<details>
					<summary>CEL condition</summary>
					<textarea
						rows="2"
						aria-label="CEL condition"
						placeholder="task.startsWith(&quot;code-&quot;) && input_tokens > 32000"
						bind:value={rule.cel}
						spellcheck="false"></textarea>
					<p class="help">
						Checked after the conditions above. Reads complexity, agent_id, task, user_id, end_user,
						session_id, template, interface, stream, input_tokens and tags. A condition that fails
						to evaluate does not match.
					</p>
				</details>
				<h4>Then</h4>
				{@render actionControls(rule.then, `r${i}`)}
			</article>
		{:else}
			<p class="muted">No rules: every call takes the default below.</p>
		{/each}
		<div>
			<button
				type="button"
				class="btn"
				onclick={() => (draft.rules = [...draft.rules, emptyRule(draft.rules.length + 1)])}
				{disabled}>Add a rule</button
			>
		</div>
	</fieldset>

	<fieldset class="section" {disabled}>
		<legend>When no rule matches</legend>
		{@render actionControls(draft.default, 'default')}
	</fieldset>

	<fieldset class="section" {disabled}>
		<legend>Failover</legend>
		<p class="help">For every rule that has no policy of its own.</p>
		{@render failoverControls(draft.failover, 'scheme-fo')}
	</fieldset>
{/if}

<style>
	.section {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		background: var(--surface);
	}

	.grid {
		display: grid;
		grid-template-columns: minmax(9rem, max-content) minmax(0, 28rem);
		align-items: center;
		gap: var(--space-2) var(--space-3);
	}

	.grid label,
	.label {
		font-size: var(--text-sm);
		font-weight: 600;
	}

	.checks {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2) var(--space-4);
		align-items: center;
	}

	.checks label,
	.inline {
		font-size: var(--text-sm);
		display: inline-flex;
		gap: var(--space-1);
		align-items: center;
	}

	.rule {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		padding: var(--space-3);
	}

	.rule h4 {
		margin: 0;
		font-size: var(--text-sm);
	}

	.rule .name {
		max-width: 16rem;
		font-family: var(--font-mono);
	}

	.grow {
		flex: 1;
	}

	.end {
		justify-content: flex-end;
	}

	.help {
		color: var(--text-muted);
		font-size: var(--text-sm);
		margin: 0;
	}

	.error {
		color: var(--danger);
		font-size: var(--text-sm);
		font-weight: 600;
	}

	.json,
	details textarea {
		font-family: var(--font-mono);
		width: 100%;
	}

	@media (max-width: 40rem) {
		.grid {
			grid-template-columns: 1fr;
		}
	}
</style>
