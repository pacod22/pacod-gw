<script lang="ts">
	import type { Row } from '#lib/report/format.ts';
	import ReportTable from './ReportTable.svelte';

	/**
	 * Calls of a report (recent, slowest, of a trace), each linked to its page; `query` carries
	 * the scope of the list to the call.
	 */
	let {
		caption,
		rows,
		empty = 'No calls.',
		query = ''
	}: { caption: string; rows: Row[]; empty?: string; query?: string } = $props();
</script>

<ReportTable
	{caption}
	{rows}
	{empty}
	key="id"
	fields={[
		'timestamp',
		'status',
		'http_status',
		'model_served',
		'end_user',
		'total_ms',
		'gateway_delay_ms',
		'cost',
		'error_type'
	]}
	labels={{
		status: 'Result',
		http_status: 'HTTP',
		model_served: 'Model',
		total_ms: 'Total',
		gateway_delay_ms: 'Gateway delay'
	}}
	href={(row) => `/insight/requests/${encodeURIComponent(String(row.id))}${query}`}
/>
