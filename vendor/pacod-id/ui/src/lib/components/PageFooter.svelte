<script lang="ts">
	import { LocaleSwitcher } from '@pacod/ui';
	import { m } from '#lib/paraglide/messages.js';
	import { getLocale } from '#lib/paraglide/runtime.js';
	import { LANGUAGE_NAMES } from '#lib/i18n.ts';
	import { currentTheme, setTheme, type Theme } from '#lib/theme.ts';
	import { realm } from '#lib/realm.svelte.ts';

	let theme = $state<Theme>(currentTheme());
	const offered = $derived(realm.value?.locales ?? [getLocale()]);
	const locales = $derived(offered.map((code) => ({ code, label: LANGUAGE_NAMES[code] ?? code })));

	function pick(t: Theme) {
		theme = t;
		setTheme(t);
	}
</script>

<div class="page-footer">
	{#if locales.length > 1}
		<LocaleSwitcher
			{locales}
			current={getLocale()}
			href={(code) => `?lang=${code}`}
			label={m.ui_language()}
		/>
	{/if}
	<div class="themes" role="group" aria-label={m.ui_theme()}>
		{#each [['light', m.ui_theme_light()], ['dark', m.ui_theme_dark()], ['system', m.ui_theme_system()]] as [value, label] (value)}
			<button
				type="button"
				class:on={theme === value}
				aria-pressed={theme === value}
				onclick={() => pick(value as Theme)}>{label}</button
			>
		{/each}
	</div>
	{#if realm.value?.terms_url || realm.value?.privacy_url}
		<div class="legal">
			{#if realm.value?.terms_url}<a href={realm.value.terms_url} rel="external noopener"
					>{m.ui_terms()}</a
				>{/if}
			{#if realm.value?.privacy_url}<a href={realm.value.privacy_url} rel="external noopener"
					>{m.ui_privacy()}</a
				>{/if}
		</div>
	{/if}
</div>

<style>
	.page-footer {
		display: flex;
		flex-wrap: wrap;
		gap: var(--pid-space-3);
		justify-content: center;
		align-items: center;
		font-size: var(--pid-text-sm);
		color: var(--pid-text-muted);
		margin-top: var(--pid-space-4);
	}

	.themes {
		display: inline-flex;
		border: 1px solid var(--pid-border);
		border-radius: var(--pid-radius-sm);
		overflow: hidden;
	}

	.themes button {
		background: transparent;
		border: 0;
		padding: var(--pid-space-1) var(--pid-space-3);
		color: var(--pid-text-muted);
		font: inherit;
		cursor: pointer;
	}

	.themes button.on {
		background: var(--pid-surface-2);
		color: var(--pid-text);
	}

	.legal {
		display: flex;
		gap: var(--pid-space-3);
	}
</style>
