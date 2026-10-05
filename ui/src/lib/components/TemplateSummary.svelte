<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import { describeParam, paramOf, REASONING, SAMPLING } from '#lib/templates.ts';

	/** The configuration of one template version, read-only. */
	let { spec }: { spec: Schemas['TemplateSpec'] } = $props();

	const tags = $derived(Object.entries(spec.tags ?? {}));
</script>

<dl class="facts">
	<dt>Model</dt>
	<dd class="mono">{spec.model || '–'}</dd>
	{#each [...SAMPLING, ...REASONING] as definition (definition.name)}
		<dt>{definition.label}</dt>
		<dd>{describeParam(paramOf(spec, definition.name))}</dd>
	{/each}
	<dt>Output-token limit</dt>
	<dd>{spec.max_output_tokens ?? 'The model’s own'}</dd>
	<dt>Timeout</dt>
	<dd>{spec.timeout_seconds ? `${spec.timeout_seconds} seconds` : 'The provider’s default'}</dd>
	<dt>Attempts</dt>
	<dd>{spec.retry_max_attempts ?? 'The default'}</dd>
	<dt>Prompt-cache markers</dt>
	<dd>{spec.prompt_cache}</dd>
	<dt>Unsupported parameters</dt>
	<dd>{spec.unsupported_params ?? 'As the workspace has it'}</dd>
	<dt>Audit mode</dt>
	<dd>{spec.audit}</dd>
	<dt>Tags</dt>
	<dd>{tags.length > 0 ? tags.map(([key, value]) => `${key}: ${value}`).join(', ') : 'None'}</dd>
	<dt>In proxy mode</dt>
	<dd>{spec.proxy}</dd>
	<dt>Ask for usage on streams</dt>
	<dd>{spec.request_stream_usage ? 'Yes' : 'No'}</dd>
</dl>
