<script lang="ts">
	import { page } from '$app/state';
	import PageHeader from '#lib/components/PageHeader.svelte';

	const title = $derived(
		page.status === 404
			? 'Not found'
			: page.status === 403
				? 'Your role does not allow this'
				: 'Something went wrong'
	);
</script>

<div class="stack-lg">
	<PageHeader {title} />
	<div class="card stack">
		<p>{page.error?.message}</p>
		<p class="muted small">
			{#if page.status === 404}
				The page or the object it shows does not exist. It may have been deleted, or the address may
				be wrong.
			{:else}
				Status {page.status}. Reload the page; if it keeps failing, look at the log of the console
				and of the core.
			{/if}
		</p>
		<div class="row">
			<a class="btn" href="/">Go to the dashboard</a>
		</div>
	</div>
</div>
