<script lang="ts">
	import { enhance } from '$app/forms';
	import Badge from '#lib/components/Badge.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import { formatValue } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const server = $derived(data.server);
	const tools = $derived(data.tools?.tools ?? []);
	const probe = $derived(data.tools?.probe ?? null);
	const base = $derived(organisationPath(params.org, 'tool-servers'));
	/** Tools waiting for an administrator: never approved, or changed since. */
	const waiting = $derived(
		tools.filter((t) => t.status === 'new' || t.status === 'changed').length
	);

	const TONES: Record<string, 'good' | 'bad' | 'warn' | 'neutral'> = {
		approved: 'good',
		new: 'neutral',
		changed: 'warn',
		gone: 'warn',
		disabled: 'bad'
	};

	function scopeOf(scope: { type: string; id?: string }): string {
		return scope.type === 'workspace' ? `Workspace ${scope.id}` : 'The organisation';
	}

	const pretty = (v: unknown) => JSON.stringify(v, null, 2);
</script>

<div class="stack-lg">
	<PageHeader
		title="Tool server {server.name}"
		description={server.description || 'An MCP server of the organisation.'}
		crumbs={[{ label: 'Tool servers', href: base }]}
	>
		{#snippet actions()}
			{#if data.mayManage}
				<form method="POST" action="?/probe" use:enhance>
					<button type="submit" class="btn">Ask for its tools now</button>
				</form>
			{/if}
		{/snippet}
	</PageHeader>
	<OutcomeNotice outcome={form} />

	<section class="card stack" aria-labelledby="server-title">
		<h2 id="server-title">Server</h2>
		<dl class="facts">
			<dt>Address</dt>
			<dd><code>{server.url}</code></dd>
			<dt>Credential header</dt>
			<dd>
				<code>{server.auth.header}</code>{server.auth.prefix
					? ` (${server.auth.prefix.trim()} …)`
					: ''}
			</dd>
			<dt>Last asked</dt>
			<dd>
				{#if probe}
					{formatValue('probed_at', probe.probed_at)}:
					{probe.ok ? `speaks ${probe.version}, ${probe.tools} tools` : `failed: ${probe.problem}`}
				{:else}
					Never. The gateway asks every few hours (Platform → Settings), or now with the button
					above.
				{/if}
			</dd>
			<dt>Workspaces</dt>
			<dd>
				{server.workspaces
					? server.workspaces.join(', ') || 'None'
					: 'Every workspace of the organisation'}
			</dd>
			<dt>Status</dt>
			<dd><Badge value={server.status} /></dd>
		</dl>
	</section>

	<section class="card stack" aria-labelledby="tools-title">
		<h2 id="tools-title">Tools</h2>
		<p class="muted">
			Callers are served approved tools only, under their exposed name, with the definition as it
			was approved. {waiting > 0 ? `${waiting} wait for an approval.` : ''}
		</p>
		{#if tools.length === 0}
			<p class="muted">
				No tools are known yet. Add a credential if the server needs one, then ask it for its tools.
			</p>
		{:else}
			<div class="table-wrap">
				<table>
					<caption class="sr-only">Tools of {server.name}</caption>
					<thead>
						<tr>
							<th scope="col">Tool</th>
							<th scope="col">Status</th>
							<th scope="col">Exposed as</th>
							<th scope="col">Definition</th>
							{#if data.mayManage}<th scope="col">Action</th>{/if}
						</tr>
					</thead>
					<tbody>
						{#each tools as t (t.tool)}
							<tr>
								<td><code>{t.tool}</code></td>
								<td><Badge value={t.status} tone={TONES[t.status]} /></td>
								<td><code>{t.pin?.exposed_name ?? t.suggested_name ?? '—'}</code></td>
								<td>
									<details>
										<summary>{t.seen ? 'As the server lists it' : 'As approved'}</summary>
										<pre>{pretty(t.seen?.definition ?? t.pin?.definition)}</pre>
										{#if t.status === 'changed' && t.pin}
											<p class="small">As approved:</p>
											<pre>{pretty(t.pin.definition)}</pre>
										{/if}
									</details>
								</td>
								{#if data.mayManage}
									<td class="stack-sm">
										{#if t.seen && t.status !== 'approved'}
											<form method="POST" action="?/approve" use:enhance class="inline-form">
												<input type="hidden" name="tool" value={t.tool} />
												<input type="hidden" name="hash" value={t.seen.hash} />
												<label class="sr-only" for="exposed-{t.tool}">Exposed name</label>
												<input
													id="exposed-{t.tool}"
													name="exposed_name"
													placeholder={t.suggested_name ?? `${server.name}__name`}
													required={t.suggested_name === null && !t.pin}
													value={t.pin?.exposed_name ?? ''}
												/>
												<button type="submit" class="btn btn-primary">Approve</button>
											</form>
										{/if}
										{#if t.pin && t.status !== 'disabled'}
											<form method="POST" action="?/disable" use:enhance>
												<input type="hidden" name="tool" value={t.tool} />
												<button type="submit" class="btn btn-quiet">Stop serving</button>
											</form>
										{/if}
									</td>
								{/if}
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</section>

	<section class="card stack" aria-labelledby="credentials-title">
		<h2 id="credentials-title">Credentials</h2>
		<p class="muted">
			Sent in <code>{server.auth.header}</code> on every call. A workspace's own credential is used for
			its callers instead of the organisation's. Secrets are never shown again.
		</p>
		{#if data.credentials.length > 0}
			<div class="table-wrap">
				<table>
					<caption class="sr-only">Credentials of {server.name}</caption>
					<thead>
						<tr>
							<th scope="col">Label</th>
							<th scope="col">For</th>
							<th scope="col">State</th>
							<th scope="col">Fingerprint</th>
							{#if data.mayManage}<th scope="col">Action</th>{/if}
						</tr>
					</thead>
					<tbody>
						{#each data.credentials as c (c.id)}
							<tr>
								<td>{c.label || '—'}</td>
								<td>{scopeOf(c.scope)}</td>
								<td><Badge value={c.state} /></td>
								<td><code>{c.fingerprint}</code></td>
								{#if data.mayManage}
									<td class="inline-form">
										<form method="POST" action="?/credentialState" use:enhance>
											<input type="hidden" name="cid" value={c.id} />
											<input
												type="hidden"
												name="state"
												value={c.state === 'current' ? 'disabled' : 'current'}
											/>
											<button type="submit" class="btn btn-quiet"
												>{c.state === 'current' ? 'Disable' : 'Make current'}</button
											>
										</form>
										<form method="POST" action="?/deleteCredential" use:enhance>
											<input type="hidden" name="cid" value={c.id} />
											<button type="submit" class="btn btn-danger">Delete</button>
										</form>
									</td>
								{/if}
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
		{#if data.mayManage}
			<form method="POST" action="?/addCredential" use:enhance class="stack-sm">
				<label>
					Secret
					<input name="secret" type="password" autocomplete="off" required />
				</label>
				<label>
					Label
					<input name="label" />
				</label>
				<label>
					For one workspace only (its id; empty: the organisation)
					<input name="workspace_id" />
				</label>
				<button type="submit" class="btn btn-primary">Add credential</button>
			</form>
		{/if}
	</section>
</div>
