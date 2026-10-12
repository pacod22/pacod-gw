<script lang="ts">
	let { data } = $props();
</script>

<h1>Fixture</h1>
{#if data.who}
	<p id="who">{data.who.email} {data.who.sub}</p>
	<form method="post" action="/auth/logout"><button>Sign out</button></form>
{:else}
	<p id="who">signed out</p>
	<!-- A server route, not a page: the client-side router must not handle it. -->
	<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- not a route of the app -->
	<a href="/auth/login?next=/" data-sveltekit-reload>Sign in</a>
{/if}
