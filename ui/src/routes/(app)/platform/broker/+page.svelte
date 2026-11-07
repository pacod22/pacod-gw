<script lang="ts">
	import { enhance } from '$app/forms';
	import Badge from '#lib/components/Badge.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import {
		DEFAULT_VISIBILITY,
		VISIBILITIES,
		editable,
		modelBlocked,
		templateBlocked
	} from '#lib/broker.ts';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	const status = $derived(
		(data.status ?? {}) as {
			configured?: boolean;
			url?: string;
			reachable?: boolean;
			error?: string | null;
			resources?: number | null;
			linked_models?: number;
			pools?: number;
		}
	);
	const linked = $derived(status.linked_models ?? 0);
	const pools = $derived(status.pools ?? 0);
	const rows = $derived(
		data.linked.map((m) => ({
			...m,
			reputation: m.score === null ? m.label || '—' : `${m.score.toFixed(2)} (${m.label})`
		}))
	);

	const offer = $derived(data.offer);
	const shown = $derived(
		offer
			? {
					models: editable(offer.models, (m) => m.model_id, modelBlocked),
					templates: editable(offer.templates, (t) => t.id, templateBlocked)
				}
			: null
	);
	const outcome = $derived(form as ActionOutcome | null | undefined);
	let busy = $state(false);

	/** Ticks or clears every box of one table that can change. */
	function tickAll(event: Event, on: boolean) {
		const section = (event.currentTarget as HTMLElement).closest('section');
		for (const box of section?.querySelectorAll<HTMLInputElement>('input[type="checkbox"]') ?? [])
			if (!box.disabled) box.checked = on;
	}

	function reach(n: number): string {
		return n === 1 ? '1 workspace' : `${n} workspaces`;
	}
</script>

<div class="stack-lg">
	<PageHeader
		title="Broker"
		description="pacod-broker, the catalogue of what can be called, at what price and how well: what the core reads from it, the repository models that are broker resources, and what each organisation offers on it."
	/>

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if !status.configured}
		<Notice tone="info" title="No broker is set up">
			<p>
				Set <code>PGW_BROKER_URL</code> (and <code>PGW_BROKER_TOKEN</code>) for the core to take
				proposals from the broker, resolve templates' pools from it and serve <code>/mcp</code>.
				gw-broker, which announces what organisations offer, is set up on its own (runbook section
				20).
			</p>
		</Notice>
	{:else}
		{#if status.reachable}
			<Notice tone="success">
				<p>
					The broker at <code>{status.url}</code> answers. {linked === 1
						? '1 model is'
						: `${linked} models are`} linked to its resources; {pools === 1
						? '1 template takes'
						: `${pools} templates take`} members from it.
				</p>
			</Notice>
		{:else}
			<Notice tone="warning" title="The broker does not answer">
				<p><code>{status.url}</code>: {status.error ?? 'no answer'}</p>
			</Notice>
		{/if}
		<p class="muted small">
			Models the broker lists at your providers appear in
			<a href="/platform/models/discovery">Discovery</a>, found by pacod-broker; nothing changes
			until you accept them.
		</p>
	{/if}

	<section class="stack" aria-labelledby="offer-heading">
		<h2 id="offer-heading">Offered on pacod-broker</h2>
		<p class="muted">
			Tick the models and templates of an organisation that gw-broker announces on pacod-broker,
			under the organisation's broker token. Consumers call a model by its name and a template as
			<code>@name</code>, with a key of a workspace that reaches it.
		</p>
		<Notice tone="info">
			<p>
				gw-broker announces only for the organisations named in its file with a broker token
				(runbook section 20.2). For any other organisation, what is ticked here does not reach the
				broker.
			</p>
		</Notice>

		<form method="GET" class="row">
			<label for="offer-org">Organisation</label>
			<select
				id="offer-org"
				name="org"
				value={data.org}
				onchange={(e) => e.currentTarget.form?.requestSubmit()}
			>
				<option value="">Choose an organisation</option>
				{#each data.organisations as o (o.value)}
					<option value={o.value}>{o.label}</option>
				{/each}
			</select>
			<noscript><button class="btn" type="submit">Show</button></noscript>
		</form>

		{#if data.offerProblem}
			<ProblemNotice problem={data.offerProblem} />
		{:else if offer && shown}
			<OutcomeNotice {outcome} always={['offer']} />
			{#key offer}
				<form
					method="POST"
					action="?/offer"
					class="stack"
					use:enhance={() => {
						busy = true;
						return async ({ update }) => {
							await update({ reset: false });
							busy = false;
						};
					}}
				>
					<input type="hidden" name="org" value={data.org} />
					<input type="hidden" name="shown" value={JSON.stringify(shown)} />

					<section class="stack" aria-labelledby="offer-models">
						<div class="row spread">
							<h3 id="offer-models">Models granted to the organisation</h3>
							{#if data.canOffer}
								<div class="row">
									<button type="button" class="btn btn-quiet" onclick={(e) => tickAll(e, true)}
										>All</button
									>
									<button type="button" class="btn btn-quiet" onclick={(e) => tickAll(e, false)}
										>None</button
									>
								</div>
							{/if}
						</div>
						<div class="card table-wrap">
							<table>
								<thead>
									<tr>
										<th><span class="visually-hidden">Offered</span></th>
										<th>Model</th>
										<th>State</th>
										<th>Called from</th>
										<th>Who finds it</th>
										<th>Summary</th>
									</tr>
								</thead>
								<tbody>
									{#each offer.models as m (m.model_id)}
										{@const why = modelBlocked(m)}
										<tr class:off={why && !m.offer}>
											<td>
												<input
													type="checkbox"
													name="model"
													value={m.model_id}
													checked={Boolean(m.offer)}
													disabled={!data.canOffer || (why !== null && !m.offer)}
													aria-label="Offer {m.name}"
												/>
											</td>
											<td>
												<code>{m.name}</code>
												{#if why}<span class="muted small block"
														>{m.offer
															? 'Offered, but left out: '
															: 'Cannot be offered: '}{why}</span
													>{/if}
											</td>
											<td><Badge value={m.state} /></td>
											<td class="small">{reach(m.reached_by.length)}</td>
											<td>
												<select
													name="visibility:{m.model_id}"
													value={m.offer?.visibility ?? DEFAULT_VISIBILITY}
													disabled={!data.canOffer}
													aria-label="Who finds {m.name}"
												>
													{#each VISIBILITIES as v (v.value)}
														<option value={v.value}>{v.label}</option>
													{/each}
												</select>
											</td>
											<td>
												<input
													name="summary:{m.model_id}"
													value={m.offer?.summary ?? ''}
													disabled={!data.canOffer}
													placeholder="A sentence naming the model"
													aria-label="Summary of {m.name}"
												/>
											</td>
										</tr>
									{:else}
										<tr
											><td colspan="6" class="muted">No model is granted to the organisation.</td
											></tr
										>
									{/each}
								</tbody>
							</table>
						</div>
					</section>

					<section class="stack" aria-labelledby="offer-templates">
						<div class="row spread">
							<h3 id="offer-templates">Templates</h3>
							{#if data.canOffer}
								<div class="row">
									<button type="button" class="btn btn-quiet" onclick={(e) => tickAll(e, true)}
										>All</button
									>
									<button type="button" class="btn btn-quiet" onclick={(e) => tickAll(e, false)}
										>None</button
									>
								</div>
							{/if}
						</div>
						<p class="muted small">
							A template's offer is its own switch, the one on its page: ticking it here writes a
							new version of the template.
						</p>
						<div class="card table-wrap">
							<table>
								<thead>
									<tr>
										<th><span class="visually-hidden">Offered</span></th>
										<th>Template</th>
										<th>Of</th>
										<th>Status</th>
										<th>Called from</th>
										<th>Who finds it</th>
										<th>Summary</th>
									</tr>
								</thead>
								<tbody>
									{#each offer.templates as t (t.id)}
										{@const why = templateBlocked(t)}
										<tr class:off={why && !t.offer}>
											<td>
												<input
													type="checkbox"
													name="template"
													value={t.id}
													checked={Boolean(t.offer)}
													disabled={!data.canOffer || (why !== null && !t.offer)}
													aria-label="Offer @{t.name}"
												/>
											</td>
											<td>
												<a
													href={t.workspace
														? `/orgs/${data.org}/workspaces/${t.workspace.id}/templates/${t.id}`
														: `/orgs/${data.org}/templates/${t.id}`}><code>@{t.name}</code></a
												>
												<span class="muted small">version {t.current_version}</span>
												{#if why}<span class="muted small block"
														>{t.offer
															? 'Offered, but left out: '
															: 'Cannot be offered: '}{why}</span
													>{/if}
											</td>
											<td class="small"
												>{t.workspace ? `Workspace ${t.workspace.name}` : 'Organisation'}</td
											>
											<td><Badge value={t.status} /></td>
											<td class="small">{reach(t.reached_by.length)}</td>
											<td>
												<select
													name="visibility:{t.id}"
													value={t.offer?.visibility ?? DEFAULT_VISIBILITY}
													disabled={!data.canOffer}
													aria-label="Who finds @{t.name}"
												>
													{#each VISIBILITIES as v (v.value)}
														<option value={v.value}>{v.label}</option>
													{/each}
												</select>
											</td>
											<td>
												<input
													name="summary:{t.id}"
													value={t.offer?.summary ?? ''}
													disabled={!data.canOffer}
													placeholder={t.description || 'A sentence naming the template'}
													aria-label="Summary of @{t.name}"
												/>
											</td>
										</tr>
									{:else}
										<tr><td colspan="7" class="muted">The organisation has no templates.</td></tr>
									{/each}
								</tbody>
							</table>
						</div>
					</section>

					{#if data.canOffer}
						<div class="row">
							<button class="btn btn-primary" type="submit" disabled={busy}
								>Save what is offered</button
							>
						</div>
					{:else}
						<p class="muted small">Only a platform admin changes what is offered.</p>
					{/if}
				</form>
			{/key}
		{/if}
	</section>

	<section class="stack" aria-labelledby="linked-heading">
		<h2 id="linked-heading">Linked models</h2>
		{#if rows.length === 0}
			<div class="card muted">
				No model of the repository is linked to a broker resource yet. Set its
				<code>broker_resource_id</code>, or accept a proposal from the broker.
			</div>
		{:else}
			<DataTable
				caption="Repository models that are broker resources"
				{rows}
				rowKey={(r) => r.id}
				columns={[
					{ key: 'name', label: 'Model', kind: 'code', href: (r) => `/platform/models/${r.id}` },
					{ key: 'state', label: 'State', kind: 'status' },
					{ key: 'resource', label: 'Broker resource', kind: 'code' },
					{ key: 'reputation', label: 'Reputation' },
					{ key: 'price', label: 'Price now' },
					{ key: 'live', label: 'Live' }
				]}
			/>
			{#if data.more > 0}<p class="muted small">And {data.more} more.</p>{/if}
		{/if}
	</section>
</div>

<style>
	.spread {
		justify-content: space-between;
	}
	.block {
		display: block;
	}
	tr.off td {
		opacity: 0.6;
	}
</style>
