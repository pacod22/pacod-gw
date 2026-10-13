<script lang="ts">
	import { AuthCard } from '@pacod/ui';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
</script>

<svelte:head>
	<title>No access yet · pacod-gw</title>
</svelte:head>

<AuthCard title="No access yet" subtitle="You are signed in as {data.display}.">
	<div class="body">
		<p>
			This gateway has no role for you yet. An administrator of an organisation can invite you, or
			give you a role.
		</p>
		<p>
			An invitation that is waiting for you is in <a href={data.invitations}>your account</a>.
		</p>
		<form method="POST" action="/logout">
			<button type="submit" class="btn">Sign out</button>
		</form>
	</div>
</AuthCard>

<style>
	.body {
		display: grid;
		gap: 0.875rem;
	}
	.body p {
		margin: 0;
	}
</style>
