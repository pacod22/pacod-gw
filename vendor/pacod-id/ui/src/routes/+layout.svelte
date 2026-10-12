<script lang="ts">
	import '@pacod/ui/tokens.css';
	import '@fontsource-variable/inter';
	import '@fontsource-variable/manrope';
	import '@fontsource-variable/figtree';
	import '@fontsource/caprasimo';
	import '../app.css';
	import { page } from '$app/state';
	import { chooseLanguage, isLocale } from '#lib/i18n.ts';

	let { children } = $props();

	// The language links of the footer add ?lang=xx: remember it, and load the page again in it.
	$effect(() => {
		const lang = page.url.searchParams.get('lang');
		if (isLocale(lang)) {
			const url = new URL(page.url.href);
			url.searchParams.delete('lang');
			history.replaceState(history.state, '', url.pathname + url.search + url.hash);
			chooseLanguage(lang);
		}
	});
</script>

{@render children()}
