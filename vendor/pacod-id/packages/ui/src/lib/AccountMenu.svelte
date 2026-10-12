<script lang="ts">
	import type { Snippet } from 'svelte';
	import { initials } from './brands.js';

	/**
	 * The signed-in person in a product's top bar: initials that open a panel with name, address,
	 * a link to the account pages on the identity service, and sign-out. A <details> element, so
	 * it opens without JavaScript.
	 */
	interface Props {
		name: string;
		email?: string;
		/** A line under the address, e.g. the role. */
		detail?: string;
		/** The identity service's account page. */
		accountHref: string;
		accountLabel?: string;
		/** Where the sign-out form posts. */
		signOutAction: string;
		signOutLabel?: string;
		/** More entries, e.g. an organisation switcher, between the account link and sign-out. */
		children?: Snippet;
	}

	let {
		name,
		email,
		detail,
		accountHref,
		accountLabel = 'Account settings',
		signOutAction,
		signOutLabel = 'Sign out',
		children
	}: Props = $props();
</script>

<details class="account">
	<summary aria-label={name}>
		<span class="avatar" aria-hidden="true">{initials(name || email || '')}</span>
	</summary>
	<div class="panel">
		<div class="who">
			<p class="name">{name}</p>
			{#if email && email !== name}<p class="email">{email}</p>{/if}
			{#if detail}<p class="detail">{detail}</p>{/if}
		</div>
		<a class="item" href={accountHref}>{accountLabel}</a>
		{#if children}{@render children()}{/if}
		<form method="POST" action={signOutAction}>
			<button type="submit" class="item">{signOutLabel}</button>
		</form>
	</div>
</details>

<style>
	.account {
		position: relative;
	}

	summary {
		display: block;
		list-style: none;
		border-radius: 50%;
		cursor: pointer;
	}

	summary::-webkit-details-marker {
		display: none;
	}

	summary:focus-visible {
		outline: 2px solid var(--pid-focus);
		outline-offset: 2px;
	}

	.avatar {
		display: grid;
		place-items: center;
		width: 2rem;
		height: 2rem;
		border-radius: 50%;
		background: var(--pid-accent-soft);
		color: var(--pid-accent);
		font-family: var(--pid-font-display);
		font-size: var(--pid-text-xs);
		font-weight: 700;
	}

	.panel {
		position: absolute;
		top: calc(100% + 0.5rem);
		right: 0;
		z-index: 20;
		display: flex;
		flex-direction: column;
		min-width: 15rem;
		padding: 0.5rem;
		border: 1px solid var(--pid-border);
		border-radius: var(--pid-radius-lg);
		background: var(--pid-surface);
		box-shadow: var(--pid-shadow);
	}

	.who {
		padding: 0.5rem 0.625rem 0.75rem;
		border-bottom: 1px solid var(--pid-border);
		margin-bottom: 0.25rem;
	}

	.who p {
		margin: 0;
		overflow-wrap: anywhere;
	}

	.name {
		font-weight: 600;
	}

	.email,
	.detail {
		color: var(--pid-text-muted);
		font-size: var(--pid-text-sm);
	}

	.item {
		display: block;
		width: 100%;
		padding: 0.5rem 0.625rem;
		border: 0;
		border-radius: var(--pid-radius-sm);
		background: none;
		color: var(--pid-text);
		font: inherit;
		font-size: var(--pid-text-sm);
		text-align: left;
		text-decoration: none;
		cursor: pointer;
	}

	.item:hover {
		background: var(--pid-surface-2);
		text-decoration: none;
	}

	form {
		margin: 0;
	}
</style>
