/**
 * One operation per key at a time, in this process: callers that arrive while it runs get its
 * result, and so do callers in the next `keepMs` (a page's parallel requests that still carry the
 * old refresh token).
 */
export class SingleFlight<T> {
	private running = new Map<string, Promise<T>>();
	private done = new Map<string, { value: T; until: number }>();

	constructor(private keepMs = 30_000) {}

	async run(key: string, fn: () => Promise<T>): Promise<T> {
		const now = Date.now();
		for (const [k, v] of this.done) if (v.until <= now) this.done.delete(k);
		const kept = this.done.get(key);
		if (kept) return kept.value;
		const inFlight = this.running.get(key);
		if (inFlight) return inFlight;
		const p = (async () => {
			try {
				const value = await fn();
				this.done.set(key, { value, until: Date.now() + this.keepMs });
				return value;
			} finally {
				this.running.delete(key);
			}
		})();
		this.running.set(key, p);
		return p;
	}
}
