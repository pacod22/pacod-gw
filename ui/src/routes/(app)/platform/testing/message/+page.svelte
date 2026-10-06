<script lang="ts">
	import { enhance } from '$app/forms';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import {
		compose,
		defaultAuthHeader,
		defaultDraft,
		dialectOf,
		INTERFACES,
		newTraceparent,
		type Draft
	} from '#lib/message.ts';
	import { formatDuration, type MessageSummary } from '#lib/testing.ts';
	import type { MessageOptions } from './options/+server.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	// svelte-ignore state_referenced_locally
	let draft: Draft = $state(data.draft ?? defaultDraft(data.orgs[0]?.id ?? ''));
	let ownKey = $state('');
	let options = $state<MessageOptions | null>(null);
	let busy = $state(false);

	const composed = $derived(compose(draft));
	const dialect = $derived(dialectOf(draft));
	const problem = $derived(form && !form.ok ? (form.problem ?? null) : null);
	const editing = $derived(draft.rawBody !== null);
	const blockAllowed = $derived(draft.interface === 'openai' || draft.interface === 'native');
	const proxyProvider = $derived(options?.providers.find((p) => p.name === draft.provider) ?? null);
	const auditNote = $derived.by(() => {
		if (!draft.gateway.audit || !options) return '';
		if (options.audit === 'off')
			return 'The workspace has audit mode off: the call asks to be recorded, but will not be.';
		if (options.audit === null)
			return 'The workspace takes its audit mode from the organisation or the platform. A call that asks to be recorded is recorded when that mode is “on request” or “always”.';
		return '';
	});

	// The lists of the chosen organisation and workspace; the rest of the form stays as it is.
	$effect(() => {
		const org = draft.org;
		const ws = draft.ws;
		const query = new URLSearchParams({ org, ws });
		const controller = new AbortController();
		fetch(`/platform/testing/message/options?${query}`, { signal: controller.signal })
			.then((r) => (r.ok ? (r.json() as Promise<MessageOptions>) : null))
			.then((answer) => {
				if (!answer) return;
				options = answer;
				if (org && !answer.workspaces.some((w) => w.id === draft.ws))
					draft.ws = answer.workspaces[0]?.id ?? '';
			})
			.catch(() => {});
		return () => controller.abort();
	});

	function chooseInterface() {
		draft.authHeader = defaultAuthHeader(draft.interface, draft.proxyPath);
		if (draft.rawBody !== null && !confirm('Keep the body you edited by hand?'))
			draft.rawBody = null;
	}

	function toggleEditing(event: Event) {
		const on = (event.currentTarget as HTMLInputElement).checked;
		if (on) {
			draft.rawBody = composed.body ?? '';
		} else if (
			confirm('Discard the changes made by hand and build the body from the form again?')
		) {
			draft.rawBody = null;
		} else {
			(event.currentTarget as HTMLInputElement).checked = true;
		}
	}

	const summaryPath = (m: MessageSummary) => `${m.method} ${m.path}`;

	// The template or model is chosen from the workspace's lists; "Another name…" lets any
	// name be typed, for example one the gateway should refuse.
	const OTHER = ':another:';
	const templates = $derived(options?.templates ?? []);
	const granted = $derived(options?.models.filter((m) => m.granted) ?? []);
	const notGranted = $derived(options?.models.filter((m) => !m.granted) ?? []);
	const listed = $derived(
		new Set([...templates.map((t) => `@${t}`), ...(options?.models ?? []).map((m) => m.name)])
	);
	let typing = $state(false);
	const modelChoice = $derived(
		typing || (draft.model !== '' && !listed.has(draft.model)) ? OTHER : draft.model
	);

	function chooseModel(event: Event) {
		const value = (event.currentTarget as HTMLSelectElement).value;
		typing = value === OTHER;
		if (!typing) draft.model = value;
	}
</script>

<PageHeader
	title="Send a message"
	description="Compose any request to the gateway's data plane, send it as a workspace, and trace it: what the gateway planned, what came back byte by byte, the request log with where the time went, and the audit record with what the gateway changed."
/>

{#if data.problem}<ProblemNotice problem={data.problem} />{/if}

<form
	method="POST"
	action="?/send"
	class="composer"
	use:enhance={() => {
		busy = true;
		return async ({ update }) => {
			await update({ reset: false });
			busy = false;
		};
	}}
>
	<input type="hidden" name="draft" value={JSON.stringify(draft)} />
	<input type="hidden" name="key_kind" value={draft.keyKind} />

	<div class="builder stack-lg">
		<section class="card stack">
			<h2>Send as</h2>
			<div class="grid">
				<div class="field">
					<label for="m-org">Organisation</label>
					<select id="m-org" bind:value={draft.org}>
						{#each data.orgs as org (org.id)}<option value={org.id}>{org.name}</option>{/each}
					</select>
				</div>
				<div class="field">
					<label for="m-ws">Workspace</label>
					<select id="m-ws" bind:value={draft.ws}>
						{#each options?.workspaces ?? [] as ws (ws.id)}<option value={ws.id}>{ws.name}</option
							>{/each}
					</select>
				</div>
			</div>
			<fieldset class="stack">
				<legend>Key</legend>
				<label class="choice">
					<input type="radio" bind:group={draft.keyKind} value="temporary" />
					<span
						>A temporary key <span class="muted small"
							>– made in the workspace for this message, deleted right after; it expires within 10
							minutes in any case</span
						></span
					>
				</label>
				<label class="choice">
					<input type="radio" bind:group={draft.keyKind} value="own" />
					<span
						>A key of my own <span class="muted small"
							>– to test what that key may do; used for this message only and never stored</span
						></span
					>
				</label>
				{#if draft.keyKind === 'own'}
					<div class="field">
						<label for="m-key">API key</label>
						<input
							id="m-key"
							name="own_key"
							type="password"
							autocomplete="off"
							spellcheck="false"
							class="mono"
							bind:value={ownKey}
							placeholder="pgw_…"
						/>
						<p class="muted small">
							Calls with wrong keys count against the tester: 60 in a minute block it for a while,
							test runs included.
						</p>
					</div>
				{/if}
			</fieldset>
		</section>

		<section class="card stack">
			<h2>Interface</h2>
			<div class="grid">
				<div class="field">
					<label for="m-interface">Interface</label>
					<select id="m-interface" bind:value={draft.interface} onchange={chooseInterface}>
						{#each INTERFACES as i (i.id)}<option value={i.id}>{i.label} – {i.path}</option>{/each}
					</select>
				</div>
				{#if draft.interface === 'proxy'}
					<div class="field">
						<label for="m-provider">Provider</label>
						<select id="m-provider" bind:value={draft.provider}>
							<option value="">Choose…</option>
							{#each options?.providers ?? [] as p (p.name)}<option value={p.name}
									>{p.name} ({p.family})</option
								>{/each}
						</select>
					</div>
					<div class="field">
						<label for="m-proxy-path">Path at the provider</label>
						<input
							id="m-proxy-path"
							class="mono"
							bind:value={draft.proxyPath}
							onchange={chooseInterface}
						/>
					</div>
				{:else if draft.interface === 'custom'}
					<div class="field">
						<label for="m-method">Method</label>
						<select id="m-method" bind:value={draft.customMethod}>
							{#each ['GET', 'POST', 'PUT', 'PATCH', 'DELETE'] as m (m)}<option value={m}
									>{m}</option
								>{/each}
						</select>
					</div>
					<div class="field">
						<label for="m-path">Path</label>
						<input id="m-path" class="mono" bind:value={draft.customPath} />
					</div>
				{/if}
				{#if dialect}
					<div class="field">
						<label for="m-model"
							>{draft.interface === 'proxy' ? "Provider's model id" : 'Template or model'}</label
						>
						{#if draft.interface === 'proxy'}
							<input id="m-model" class="mono" bind:value={draft.model} placeholder="gpt-…" />
						{:else}
							<select id="m-model" value={modelChoice} onchange={chooseModel}>
								<option value="" disabled>Choose…</option>
								{#if templates.length > 0}
									<optgroup label="Templates of this workspace">
										{#each templates as t (t)}<option value="@{t}">@{t}</option>{/each}
									</optgroup>
								{/if}
								{#if granted.length > 0}
									<optgroup label="Models granted to the organisation">
										{#each granted as m (m.name)}<option value={m.name}>{m.name}</option>{/each}
									</optgroup>
								{/if}
								{#if notGranted.length > 0}
									<optgroup label="Other active models – the gateway refuses them here">
										{#each notGranted as m (m.name)}<option value={m.name}>{m.name}</option>{/each}
									</optgroup>
								{/if}
								<option value={OTHER}>Another name…</option>
							</select>
							{#if modelChoice === OTHER}
								<input
									aria-label="Template or model name"
									class="mono"
									bind:value={draft.model}
									placeholder="provider/model or @template"
								/>
							{/if}
							{#if options && templates.length === 0}
								<p class="muted small">This workspace has no templates.</p>
							{/if}
						{/if}
					</div>
				{/if}
				<div class="field">
					<label for="m-auth">The key goes in</label>
					<select id="m-auth" bind:value={draft.authHeader}>
						<option value="authorization">Authorization: Bearer …</option>
						<option value="x-api-key">x-api-key</option>
						<option value="x-goog-api-key">x-goog-api-key</option>
					</select>
				</div>
			</div>
			{#if draft.interface === 'proxy' && proxyProvider}
				<p class="muted small">
					The provider speaks the {proxyProvider.family} dialect. Proxy mode relays the body unchanged:
					name the model as the provider knows it.
				</p>
			{/if}
		</section>

		{#if dialect}
			<section class="card stack" class:off={editing}>
				<h2>Conversation</h2>
				<div class="field">
					<label for="m-system">System prompt</label>
					<textarea id="m-system" rows="2" bind:value={draft.system} disabled={editing}></textarea>
				</div>
				<ol class="messages">
					{#each draft.messages as message, i (i)}
						<li class="message">
							<select
								aria-label="Role of message {i + 1}"
								bind:value={message.role}
								disabled={editing}
							>
								<option value="user">user</option>
								<option value="assistant">assistant</option>
							</select>
							<textarea
								aria-label="Message {i + 1}"
								rows="2"
								bind:value={message.content}
								disabled={editing}></textarea>
							<button
								type="button"
								class="btn btn-quiet danger"
								disabled={editing || draft.messages.length === 1}
								onclick={() => draft.messages.splice(i, 1)}
								>Remove<span class="visually-hidden"> message {i + 1}</span></button
							>
						</li>
					{/each}
				</ol>
				<div>
					<button
						type="button"
						class="btn"
						disabled={editing}
						onclick={() =>
							draft.messages.push({
								role: draft.messages.at(-1)?.role === 'user' ? 'assistant' : 'user',
								content: ''
							})}>Add a message</button
					>
				</div>
			</section>

			<section class="card stack" class:off={editing}>
				<h2>Parameters</h2>
				<div class="grid">
					<div class="field">
						<label for="m-max"
							>{dialect === 'anthropic'
								? 'max_tokens'
								: dialect === 'native'
									? 'max_output_tokens'
									: 'max_tokens'}</label
						>
						<input
							id="m-max"
							inputmode="numeric"
							bind:value={draft.maxTokens}
							disabled={editing}
							placeholder={dialect === 'anthropic' ? '1024' : 'the model’s default'}
						/>
					</div>
					<div class="field">
						<label for="m-temp">temperature</label>
						<input
							id="m-temp"
							inputmode="decimal"
							bind:value={draft.temperature}
							disabled={editing}
						/>
					</div>
					<div class="field">
						<label for="m-top-p">top_p</label>
						<input id="m-top-p" inputmode="decimal" bind:value={draft.topP} disabled={editing} />
					</div>
					<div class="field">
						<label for="m-stop">Stop sequences, one per line</label>
						<textarea id="m-stop" rows="2" bind:value={draft.stop} disabled={editing}></textarea>
					</div>
				</div>
				<label class="choice">
					<input type="checkbox" bind:checked={draft.stream} disabled={editing} />
					<span>Stream the answer</span>
				</label>
				{#if draft.stream && dialect === 'openai'}
					<label class="choice">
						<input type="checkbox" bind:checked={draft.includeUsage} disabled={editing} />
						<span
							>Ask for usage on the last chunk <span class="muted small">(stream_options)</span
							></span
						>
					</label>
				{/if}
				<div class="field">
					<label for="m-extra">Additional body fields, as a JSON object</label>
					<textarea
						id="m-extra"
						rows="3"
						class="mono"
						spellcheck="false"
						bind:value={draft.extraBody}
						disabled={editing}
						placeholder={'{"tools": [...], "response_format": {...}}'}></textarea>
				</div>
			</section>
		{/if}

		<section class="card stack">
			<h2>Gateway options</h2>
			<div class="grid">
				<div class="field">
					<label for="g-template">Template</label>
					<input id="g-template" list="g-templates" bind:value={draft.gateway.template} />
					<datalist id="g-templates">
						{#each options?.templates ?? [] as t (t)}<option value={t}></option>{/each}
					</datalist>
				</div>
				<div class="field">
					<label for="g-end-user">End user</label>
					<input id="g-end-user" bind:value={draft.gateway.endUser} />
				</div>
				<div class="field">
					<label for="g-session">Session</label>
					<input id="g-session" bind:value={draft.gateway.session} />
				</div>
				<div class="field">
					<label for="g-trace">Trace id</label>
					<input id="g-trace" bind:value={draft.gateway.trace} />
				</div>
				<div class="field">
					<label for="g-agent">Agent</label>
					<input id="g-agent" bind:value={draft.gateway.agentId} />
				</div>
				<div class="field">
					<label for="g-user">User</label>
					<input id="g-user" bind:value={draft.gateway.userId} />
				</div>
				<div class="field">
					<label for="g-task">Task</label>
					<input id="g-task" bind:value={draft.gateway.task} />
				</div>
				<div class="field">
					<label for="g-complexity">Complexity</label>
					<select id="g-complexity" bind:value={draft.gateway.complexity}>
						<option value="">Not given</option>
						<option value="low">low</option>
						<option value="medium">medium</option>
						<option value="high">high</option>
					</select>
				</div>
				<div class="field">
					<label for="g-tags">Tags, key=value, …</label>
					<input id="g-tags" bind:value={draft.gateway.tags} placeholder="purpose=test" />
				</div>
				<div class="field">
					<label for="g-timeout">Gateway timeout, ms</label>
					<input id="g-timeout" inputmode="numeric" bind:value={draft.gateway.timeoutMs} />
				</div>
				<div class="field">
					<label for="g-retries">Attempts</label>
					<input id="g-retries" inputmode="numeric" bind:value={draft.gateway.retries} />
				</div>
				<div class="field">
					<label for="g-unsupported">Unsupported parameters</label>
					<select id="g-unsupported" bind:value={draft.gateway.unsupportedParams}>
						<option value="">The workspace's choice</option>
						<option value="drop">drop</option>
						<option value="reject">reject</option>
					</select>
				</div>
				<div class="field">
					<label for="g-cache">Prompt cache</label>
					<select id="g-cache" bind:value={draft.gateway.promptCache}>
						<option value="">The workspace's choice</option>
						<option value="off">off</option>
						<option value="passthrough">passthrough</option>
						<option value="auto">auto</option>
					</select>
				</div>
			</div>
			<label class="choice">
				<input type="checkbox" bind:checked={draft.gateway.audit} />
				<span
					>Ask for an audit record <span class="muted small"
						>(x-gw-audit) – the trace then shows the four bodies and what the gateway changed</span
					></span
				>
			</label>
			{#if auditNote}<p class="muted small">{auditNote}</p>{/if}
			{#if blockAllowed}
				<fieldset class="row">
					<legend>Send the options</legend>
					<label class="choice"
						><input
							type="radio"
							bind:group={draft.gatewayIn}
							value="headers"
							disabled={editing}
						/><span>as x-gw-* headers</span></label
					>
					<label class="choice"
						><input
							type="radio"
							bind:group={draft.gatewayIn}
							value="body"
							disabled={editing}
						/><span>in the body's gateway block</span></label
					>
				</fieldset>
			{/if}
			<div class="field">
				<label for="g-traceparent">traceparent (W3C)</label>
				<div class="row">
					<input
						id="g-traceparent"
						class="mono grow"
						bind:value={draft.traceparent}
						placeholder="00-…-…-01"
					/>
					<button type="button" class="btn" onclick={() => (draft.traceparent = newTraceparent())}
						>New</button
					>
				</div>
			</div>
		</section>

		<section class="card stack">
			<h2>More headers</h2>
			{#each draft.extraHeaders as row, i (i)}
				<div class="row header-row">
					<input
						aria-label="Header name {i + 1}"
						class="mono"
						bind:value={row.name}
						placeholder="name"
					/>
					<input
						aria-label="Header value {i + 1}"
						class="mono grow"
						bind:value={row.value}
						placeholder="value"
					/>
					<button
						type="button"
						class="btn btn-quiet danger"
						onclick={() => draft.extraHeaders.splice(i, 1)}
						>Remove<span class="visually-hidden"> header {i + 1}</span></button
					>
				</div>
			{/each}
			<div>
				<button
					type="button"
					class="btn"
					onclick={() => draft.extraHeaders.push({ name: '', value: '' })}>Add a header</button
				>
			</div>
			<div class="field narrow">
				<label for="m-timeout">Wait for the answer, seconds</label>
				<input id="m-timeout" type="number" min="1" max="300" bind:value={draft.timeoutS} />
			</div>
		</section>
	</div>

	<aside class="preview card stack">
		<h2>The request</h2>
		<p class="mono request-line">{composed.method} {composed.path}</p>
		<ul class="headers mono small">
			<li>
				{draft.authHeader === 'authorization' ? 'authorization: Bearer' : `${draft.authHeader}:`}
				<span class="muted">{draft.keyKind === 'own' ? 'your key' : 'the temporary key'}</span>
			</li>
			{#each Object.entries(composed.headers) as [name, value] (name)}<li>
					{name}: {value}
				</li>{/each}
		</ul>
		{#if composed.body !== null || editing}
			<label class="choice">
				<input type="checkbox" checked={editing} onchange={toggleEditing} />
				<span
					>Edit the body by hand <span class="muted small">– the form above stops building it</span
					></span
				>
			</label>
			{#if editing}
				<textarea
					aria-label="Body"
					class="mono body"
					rows="20"
					spellcheck="false"
					bind:value={draft.rawBody}></textarea>
			{:else}
				<pre class="body">{composed.body}</pre>
			{/if}
		{:else}
			<p class="muted small">No body.</p>
		{/if}
		{#each composed.notes as note (note)}<p class="small warning">{note}</p>{/each}
		{#if composed.problems.length > 0}
			<Notice tone="warning" title="Not ready to send">
				<ul>
					{#each composed.problems as p (p)}<li>{p}</li>{/each}
				</ul>
			</Notice>
		{/if}
		{#if problem}<ProblemNotice {problem} />{/if}
		<div class="row">
			<button
				type="submit"
				class="btn btn-primary"
				disabled={busy || composed.problems.length > 0 || !draft.ws}
				>{busy ? 'Waiting for the answer…' : 'Send and trace'}</button
			>
			<button
				type="button"
				class="btn btn-quiet"
				disabled={busy}
				onclick={() => {
					if (confirm('Start again with an empty form?')) {
						const { org, ws } = draft;
						draft = defaultDraft(org, ws);
					}
				}}>Reset</button
			>
		</div>
		<p class="muted small">
			A message is a real call: it is admitted against the workspace's limits, counted and billed.
		</p>
	</aside>
</form>

<section class="stack recent">
	<h2>Recent messages</h2>
	{#if data.recent.length === 0}
		<div class="card muted">No message sent yet.</div>
	{:else}
		<DataTable
			caption="Recent messages"
			rows={data.recent}
			columns={[
				{
					key: 'created_at',
					label: 'Sent (UTC)',
					kind: 'datetime',
					href: (m) => `/platform/testing/message/${m.id}`
				},
				{ key: 'request', label: 'Request', kind: 'code', value: summaryPath },
				{
					key: 'status',
					label: 'Answer',
					value: (m) => (m.error ? m.error : String(m.status))
				},
				{ key: 'model_served', label: 'Model served', kind: 'code', empty: '–' },
				{
					key: 'total_ms',
					label: 'Took',
					kind: 'number',
					value: (m) => formatDuration(m.total_ms),
					sortValue: (m) => m.total_ms
				},
				{ key: 'started_by', label: 'Sent by', value: (m) => m.started_by.display }
			]}
		/>
	{/if}
</section>

<style>
	.composer {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 30rem);
		gap: var(--space-4);
		align-items: start;
	}

	@media (max-width: 72rem) {
		.composer {
			grid-template-columns: minmax(0, 1fr);
		}
	}

	.preview {
		position: sticky;
		top: var(--space-4);
		max-height: calc(100vh - 2 * var(--space-4));
		overflow-y: auto;
	}

	@media (max-width: 72rem) {
		.preview {
			position: static;
			max-height: none;
		}
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(13rem, 1fr));
		gap: var(--space-3);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}

	.narrow {
		max-width: 16rem;
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

	.off {
		opacity: 0.6;
	}

	.messages {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.message {
		display: grid;
		grid-template-columns: 8rem minmax(0, 1fr) auto;
		gap: var(--space-2);
		align-items: start;
	}

	@media (max-width: 40rem) {
		.message {
			grid-template-columns: minmax(0, 1fr);
		}
	}

	.header-row input:first-child {
		width: 14rem;
	}

	.grow {
		flex: 1 1 12rem;
		min-width: 0;
	}

	.request-line {
		overflow-wrap: anywhere;
		font-weight: 600;
	}

	.headers {
		margin: 0;
		padding: 0;
		list-style: none;
		overflow-wrap: anywhere;
	}

	pre.body,
	textarea.body {
		max-height: 32rem;
		margin: 0;
		padding: var(--space-3);
		overflow: auto;
		border-radius: var(--radius);
		background: var(--surface-2);
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.warning {
		color: var(--warning);
	}

	.recent {
		margin-top: var(--space-5);
	}

	ul {
		padding-left: var(--space-5);
	}

	.headers {
		padding-left: 0;
	}
</style>
