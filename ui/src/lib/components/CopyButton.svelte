<script lang="ts">
	/** Puts a value on the clipboard, for ids and links that are copied rather than read. */
	interface Props {
		value: string;
		label?: string;
		/** Said to screen readers after the label: what is copied. */
		what?: string;
		class?: string;
	}

	let { value, label = 'Copy', what = '', class: className = 'btn btn-quiet' }: Props = $props();

	let copied = $state(false);
	let timer: ReturnType<typeof setTimeout> | undefined;

	async function copy() {
		try {
			await navigator.clipboard.writeText(value);
			copied = true;
			clearTimeout(timer);
			timer = setTimeout(() => (copied = false), 2000);
		} catch {
			// No clipboard access: show the value so that it can be copied by hand.
			window.prompt('Copy this value:', value);
		}
	}
</script>

<button type="button" class={className} onclick={copy}>
	{copied ? 'Copied' : label}{#if what}<span class="visually-hidden"> {what}</span>{/if}
</button>
<span class="visually-hidden" role="status">{copied ? 'Copied to the clipboard.' : ''}</span>
