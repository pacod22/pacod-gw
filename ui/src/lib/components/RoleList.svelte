<script lang="ts">
	import { describeRole } from '#lib/access.ts';
	import type { HeldRole } from '#lib/server/roles.ts';

	/** The roles of a user or an admin token, one per line. Nothing given: they are unknown. */
	let { roles }: { roles: HeldRole[] | undefined } = $props();
</script>

{#if roles === undefined}
	<span class="muted">–</span>
{:else if roles.length === 0}
	<span class="muted">No role</span>
{:else}
	<ul>
		{#each roles as role (role.binding_id)}<li>{describeRole(role)}</li>{/each}
	</ul>
{/if}

<style>
	ul {
		padding: 0;
		list-style: none;
	}
</style>
