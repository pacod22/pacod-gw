<script lang="ts">
	import { page } from '$app/state';

	const unavailable = $derived(page.status === 502 || page.status === 503);
</script>

<svelte:head>
	<title>{page.status} · pacod-gw</title>
</svelte:head>

<main>
	<div class="card stack">
		<p class="muted small">pacod-gw console · error {page.status}</p>
		<h1>
			{unavailable
				? 'The gateway does not answer'
				: page.status === 404
					? 'Page not found'
					: 'Something went wrong'}
		</h1>
		<p>{page.error?.message}</p>
		{#if unavailable}
			<p class="muted">
				The console could not reach the management API of the gateway core. Traffic through the
				gateway is not affected by the console. Check that the core is running, then reload.
			</p>
		{/if}
		<div class="row">
			<a class="btn btn-primary" href={page.url.pathname + page.url.search} data-sveltekit-reload>
				Reload
			</a>
			<a class="btn" href="/">Go to the dashboard</a>
		</div>
	</div>
</main>

<style>
	main {
		display: grid;
		place-items: center;
		min-height: 100dvh;
		padding: var(--space-4);
	}

	.card {
		width: min(34rem, 100%);
		padding: var(--space-5);
	}
</style>
