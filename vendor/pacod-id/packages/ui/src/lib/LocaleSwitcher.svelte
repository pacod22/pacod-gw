<script lang="ts">
	/**
	 * The page's language, as links: each one leads to the same page in another language, so it
	 * works without JavaScript and keeps the address shareable.
	 */
	interface Props {
		locales: { code: string; label: string }[];
		current: string;
		/** The address of the page in a language. */
		href: (code: string) => string;
		label?: string;
	}

	let { locales, current, href, label = 'Language' }: Props = $props();
</script>

<nav class="locales" aria-label={label}>
	{#each locales as l (l.code)}
		<a
			href={href(l.code)}
			hreflang={l.code}
			lang={l.code}
			aria-current={l.code === current ? 'true' : undefined}>{l.label}</a
		>
	{/each}
</nav>

<style>
	.locales {
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 0.25rem 0.75rem;
		font-size: var(--pid-text-xs);
	}

	a {
		color: var(--pid-text-muted);
		text-decoration: none;
	}

	a:hover {
		color: var(--pid-text);
		text-decoration: underline;
	}

	a[aria-current='true'] {
		color: var(--pid-text);
		font-weight: 600;
	}
</style>
