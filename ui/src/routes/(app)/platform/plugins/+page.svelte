<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import DataTable from '#lib/components/DataTable.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { Column } from '#lib/components/table.ts';
	import type { PageProps } from './$types';

	type Info = Schemas['PluginInfo'];

	let { data, form }: PageProps = $props();

	const egress = (e: Info['engines'][number]) =>
		e.egress === 'third_party'
			? `${e.id}: sends text to ${e.third_party}`
			: e.egress === 'provider'
				? `${e.id}: to a model’s provider`
				: `${e.id}: stays here`;

	const builtColumns: Column<Info>[] = [
		{ key: 'id', label: 'Plugin', kind: 'code' },
		{ key: 'title', label: 'Name' },
		{ key: 'description', label: 'What it does' },
		{ key: 'engines', label: 'Engines', value: (p) => p.engines.map(egress).join('; ') },
		{
			key: 'installed',
			label: 'Installed',
			value: (p) => (p.installed ? (p.enabled ? 'Yes' : 'Switched off') : 'No')
		},
		{ key: 'version', label: 'Version' }
	];
</script>

<ResourcePage
	title="Plugins"
	description="Plugins are built into the gateway and do nothing until the platform installs them; organisations and workspaces then switch them on. Installing a plugin also approves the third parties its engines may send text to."
	{data}
	{form}
	columns={[
		{ key: 'plugin', label: 'Plugin', kind: 'code' },
		{ key: 'enabled', label: 'Enabled', kind: 'bool' },
		{ key: 'engines', label: 'Engines allowed', kind: 'list', empty: 'All' },
		{ key: 'third_parties', label: 'Approved third parties', kind: 'list', empty: 'None' }
	]}
	nameOf={(i) => i.plugin}
	empty="No plugin is installed. Install one to let organisations and workspaces switch it on."
	deleteNote="Bindings of the plugin are kept but do nothing until it is installed again."
>
	<section class="stack">
		<h2>Built into this gateway</h2>
		{#if data.pluginsProblem}
			<ProblemNotice problem={data.pluginsProblem} />
		{:else}
			<DataTable
				caption="Plugins built into this gateway"
				columns={builtColumns}
				rows={data.plugins}
				rowKey={(p) => p.id}
			/>
		{/if}
	</section>
</ResourcePage>
