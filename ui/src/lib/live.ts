/**
 * Live updates. Listens to the console's `/events` route, which passes on the core's stream
 * of configuration changes, and reloads the data of the page that is shown.
 */
import { refreshAll } from '$app/navigation';

export type LiveState = 'connecting' | 'live' | 'offline';

export interface LiveOptions {
	/** Changes that arrive within this time are answered with one reload. */
	debounceMs?: number;
	onstate?: (state: LiveState) => void;
}

/** Starts listening. Returns the function that stops it. */
export function startLiveUpdates(options: LiveOptions = {}): () => void {
	const debounceMs = options.debounceMs ?? 400;
	const source = new EventSource('/events');
	let timer: ReturnType<typeof setTimeout> | undefined;
	let pending = false;

	const refresh = () => {
		timer = undefined;
		// Not while the tab is hidden, and not under a form someone is filling in or a row
		// menu someone has open: the list stays as it was and the reload happens once it is
		// closed.
		if (
			document.visibilityState !== 'visible' ||
			document.querySelector('dialog[open], [popover]:popover-open')
		) {
			pending = true;
			return;
		}
		pending = false;
		void refreshAll();
	};

	const schedule = () => {
		clearTimeout(timer);
		timer = setTimeout(refresh, debounceMs);
	};

	const resume = () => {
		if (pending) schedule();
	};

	source.addEventListener('open', () => options.onstate?.('live'));
	source.addEventListener('error', () => {
		// The browser reconnects by itself unless the stream was refused.
		options.onstate?.(source.readyState === EventSource.CLOSED ? 'offline' : 'connecting');
	});
	source.addEventListener('config', schedule);
	document.addEventListener('visibilitychange', resume);
	// A dialog that closes does not announce it to the document: look again now and then.
	const poll = setInterval(resume, 2000);

	return () => {
		clearTimeout(timer);
		clearInterval(poll);
		document.removeEventListener('visibilitychange', resume);
		source.close();
	};
}
