<script lang="ts">
	import { enhance } from '$app/forms';
	import PresetPicker from '#lib/components/PresetPicker.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const presets = $derived(
		data.presets.map((p) => ({
			name: p.name,
			label: `${p.display_name} (${p.family})`,
			notes: p.notes,
			values: {
				name: p.name,
				display_name: p.display_name,
				family: p.family,
				base_url: p.base_url,
				// Only where the provider takes its key other than its family does (Ollama Cloud).
				...(p.auth ? { auth: p.auth } : {}),
				managed_api: p.managed_api,
				platform: p.platform,
				quirks: p.quirks,
				// The holiday calendar its off-peak hours include (DeepSeek: Chinese public holidays).
				...(p.holidays ? { holidays: p.holidays } : {})
			}
		}))
	);
</script>

<ResourcePage
	title="Providers"
	description="The LLM provider endpoints the gateway sends requests to. Each needs a credential before it can be used."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code' },
		{ key: 'display_name', label: 'Display name' },
		{ key: 'family', label: 'Family' },
		{ key: 'base_url', label: 'Base URL' },
		{ key: 'default_timeout_seconds', label: 'Timeout (s)', kind: 'number' },
		{ key: 'status', label: 'Status', kind: 'status' }
	]}
	empty="No providers yet. Add the first provider, then give it a credential on its Credentials page."
	deleteNote="Requests that are routed to this provider will fail."
>
	{#snippet formIntro(mode)}
		{#if mode === 'create'}
			<PresetPicker
				label="Start from a preset"
				help="Choosing one of the providers the gateway knows fills the name, the family, the base URL and the quirks. Everything stays editable."
				{presets}
			/>
		{/if}
	{/snippet}
	{#snippet rowActions(item)}
		<form method="POST" action="?/probe" use:enhance class="inline">
			<input type="hidden" name="id" value={item.id} />
			<input type="hidden" name="name" value={item.name} />
			<button type="submit" class="btn btn-quiet">
				Test<span class="visually-hidden"> {item.name}</span>
			</button>
		</form>
		<a class="btn btn-quiet" href="/platform/providers/{item.id}/discovery">
			Discovery<span class="visually-hidden"> for {item.name}</span>
		</a>
		<a class="btn btn-quiet" href="/platform/providers/{item.id}/credentials">
			Credentials<span class="visually-hidden"> of {item.name}</span>
		</a>
	{/snippet}
</ResourcePage>
