<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import { enhance } from '$app/forms';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { pluginTabs } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	/** A text setting of a binding, for the list. */
	const settingOf = (b: Schemas['PluginBinding'], name: 'engine' | 'catalogue') =>
		String((b.settings as Record<string, unknown> | null)?.[name] ?? '');
	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);

	type Tried = Schemas['IntentTryResult'] & { text: string };
	const tried = $derived(form?.ok && form.action === 'try' ? (form.data as Tried) : null);
	const tryProblem = $derived(
		form && !form.ok && form.action === 'try' ? (form.problem ?? null) : null
	);
	const scored = $derived(
		form?.ok && form.action === 'evaluate' ? (form.data as Schemas['IntentEvaluation']) : null
	);
	const scoreProblem = $derived(
		form && !form.ok && form.action === 'evaluate' ? (form.problem ?? null) : null
	);
	let busy = $state(false);
	const keep = () => {
		busy = true;
		return async ({ update }: { update: (o: { reset: boolean }) => Promise<void> }) => {
			await update({ reset: false });
			busy = false;
		};
	};
	const percent = (v: number | null | undefined) =>
		v === null || v === undefined ? '–' : `${Math.round(v * 100)} %`;
	const flags = (f: Record<string, number>) =>
		Object.entries(f)
			.map(([k, v]) => `${k} ${v.toFixed(2)}`)
			.join(', ') || '–';
</script>

<div class="stack-lg">
	<Tabs label="Plugins" tabs={pluginTabs(params.org, params.ws)} />
	<ResourcePage
		title="Plugins of {workspace}"
		description="Plugins switched on for this workspace. A binding here replaces the organisation’s for the same plugin, unless the organisation locked it; mode off switches the organisation’s off here."
		{data}
		{form}
		columns={[
			{ key: 'plugin', label: 'Plugin', kind: 'code' },
			{ key: 'mode', label: 'Mode' },
			{ key: 'engine', label: 'Engine', value: (b) => settingOf(b, 'engine') || 'llm' },
			{ key: 'catalogue', label: 'Catalogue', value: (b) => settingOf(b, 'catalogue') },
			{ key: 'on_error', label: 'On error' },
			{ key: 'expose', label: 'Intent shown to callers', kind: 'bool' }
		]}
		nameOf={(b) => b.plugin}
		empty="This workspace binds no plugin of its own. Plugins its organisation switches on still apply."
		deleteNote="The organisation’s binding of the plugin, if there is one, applies again."
	/>

	<section class="card stack">
		<h2>Try the intent plugin</h2>
		<p class="muted">
			Classifies a text as a user's message with this workspace's intent binding, with each engine
			chosen, and shows what its rules would do. The engines' calls are real: they cost money and
			count against the workspace's limits.
		</p>
		<form method="POST" action="?/try" class="stack" use:enhance={keep}>
			<label class="field">
				Text
				<textarea name="text" rows="3" required value={tried?.text ?? ''}></textarea>
			</label>
			<fieldset class="engines">
				<legend>Engines (none: the binding's)</legend>
				<label><input type="checkbox" name="engines" value="llm" /> LLM</label>
				<label><input type="checkbox" name="engines" value="jev" /> Jev</label>
				<label><input type="checkbox" name="engines" value="cascade" /> Cascade</label>
			</fieldset>
			<div><button type="submit" class="btn" disabled={busy}>Classify</button></div>
		</form>
		{#if tryProblem}<ProblemNotice problem={tryProblem} />{/if}
		{#if tried}
			<table class="table">
				<caption class="visually-hidden">What each engine found</caption>
				<thead>
					<tr>
						<th>Engine</th><th>Intent</th><th>Confidence</th><th>Flags</th><th>Rules</th><th
							>Time</th
						>
					</tr>
				</thead>
				<tbody>
					{#each tried.answers as a (a.engine)}
						<tr>
							<td>{a.engine}</td>
							<td>{a.error ? '–' : (a.intent ?? '–')}</td>
							<td>{percent(a.confidence)}</td>
							<td>{flags(a.flags)}</td>
							<td>
								{#if a.error}<span class="failed">{a.error}</span>
								{:else}
									{a.action === 'block' ? `block (${a.rule}): ${a.message}` : 'allow'}{a.flagged
										? '; flagged'
										: ''}{a.trial.length ? `; in trial: ${a.trial.join(', ')}` : ''}
								{/if}
							</td>
							<td>{Math.round(a.elapsed_ms)} ms</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>

	<section class="card stack">
		<h2>Evaluate a labelled set</h2>
		<p class="muted">
			One sample per line: the expected intent, a tab, and the text; or a JSON object with
			<code>text</code> and <code>intent</code>. Up to 200 samples, each classified with each engine
			chosen.
		</p>
		<form method="POST" action="?/evaluate" class="stack" use:enhance={keep}>
			<label class="field">
				Samples
				<textarea name="samples" rows="6" required spellcheck="false"></textarea>
			</label>
			<fieldset class="engines">
				<legend>Engines (none: the binding's)</legend>
				<label><input type="checkbox" name="engines" value="llm" /> LLM</label>
				<label><input type="checkbox" name="engines" value="jev" /> Jev</label>
				<label><input type="checkbox" name="engines" value="cascade" /> Cascade</label>
			</fieldset>
			<div><button type="submit" class="btn" disabled={busy}>Evaluate</button></div>
		</form>
		{#if scoreProblem}<ProblemNotice problem={scoreProblem} />{/if}
		{#if scored}
			{#each scored.engines as e (e.engine)}
				<h3>{e.engine}: {percent(e.accuracy)} correct</h3>
				<p class="muted">
					{e.correct} of {e.samples} samples, {e.errors} without an answer; median
					{Math.round(e.latency_p50_ms)} ms, 95th percentile {Math.round(e.latency_p95_ms)} ms.
				</p>
				<table class="table">
					<caption class="visually-hidden">Per intent, {e.engine}</caption>
					<thead>
						<tr><th>Intent</th><th>Samples</th><th>Found</th><th>Precision</th><th>Recall</th></tr>
					</thead>
					<tbody>
						{#each Object.entries(e.per_intent) as [intent, s] (intent)}
							<tr>
								<td>{intent}</td>
								<td>{s.support}</td>
								<td>{s.correct}</td>
								<td>{percent(s.precision)}</td>
								<td>{percent(s.recall)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
				{#if e.wrong.length}
					<details>
						<summary>Wrong answers</summary>
						<ul>
							{#each e.wrong as w, i (i)}
								<li>“{w.text}”: expected {w.expected}, got {w.got}</li>
							{/each}
						</ul>
					</details>
				{/if}
			{/each}
		{/if}
	</section>
</div>

<style>
	.field {
		display: grid;
		gap: var(--space-1);
	}

	.failed {
		color: var(--danger);
	}

	.engines {
		display: flex;
		gap: var(--space-4);
		border: none;
		padding: 0;
	}
</style>
