import type { IdSession } from '@pacod/id-sveltekit';

declare global {
	namespace App {
		interface Locals {
			session: IdSession | null;
		}
	}
}

export {};
