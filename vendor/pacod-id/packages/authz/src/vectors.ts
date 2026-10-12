// Runs a file of test vectors (policy/engine-vectors/README.md): the engine's own, or a
// product's (authz/vectors/*.yaml next to its manifest).

import { parse } from 'yaml';
import { Catalog, type HeldBinding, type Subject } from './catalog.js';
import type { Manifest } from './manifest.js';

interface Check {
	expect: unknown;
	[key: string]: unknown;
}

/** One check: null when it holds, else the engine's answer. */
function one(c: Catalog, subject: Subject, check: Check): string | null {
	const bool = (got: boolean) => (got === check.expect ? null : String(got));
	if ('allowed' in check)
		return bool(c.allowed(subject, check.allowed as string, check.at as string[]));
	if ('has_role' in check)
		return bool(c.hasRole(subject, check.has_role as string, check.scope as string));
	if ('holds_any_at' in check) return bool(c.holdsAnyAt(subject, check.holds_any_at as string));
	if ('allowed_somewhere' in check)
		return bool(c.allowedSomewhere(subject, check.allowed_somewhere as string));
	if ('touches' in check) return bool(c.touches(subject, check.touches as string[]));
	if ('can_grant' in check) {
		const got = c.canGrant(subject, check.can_grant as string, check.at as string[]) ?? 'ok';
		return got === check.expect ? null : got;
	}
	if ('check_protected' in check) {
		const { before, after } = check.check_protected as {
			before: HeldBinding[];
			after: HeldBinding[];
		};
		const got = c.checkProtected(subject, before, after);
		const want =
			check.expect === 'ok'
				? null
				: (check.expect as { violation: { role: string; scope: string } }).violation;
		const holds =
			got === null
				? want === null
				: want !== null && got.role === want.role && got.scope === want.scope;
		return holds ? null : JSON.stringify(got);
	}
	throw new Error(`unknown check ${JSON.stringify(check)}`);
}

/**
 * Runs every check of a vector file. `load` reads a manifest by the name the file lists under
 * `manifests`. Returns the number of checks and every failure with its case.
 */
export function runVectors(
	text: string,
	load: (name: string) => Manifest
): { checks: number; failures: string[] } {
	const file = parse(text) as {
		manifests: string[];
		cases: { name: string; subject?: Subject; actor?: Subject; checks: Check[] }[];
	};
	const c = new Catalog();
	for (const name of file.manifests) c.insert(load(name));
	let checks = 0;
	const failures: string[] = [];
	for (const kase of file.cases) {
		const subject = (kase.subject ?? kase.actor) as Subject;
		for (const check of kase.checks) {
			checks++;
			try {
				const got = one(c, subject, check);
				if (got !== null) failures.push(`${kase.name}: ${JSON.stringify(check)} gave ${got}`);
			} catch (e) {
				failures.push(`${kase.name}: ${(e as Error).message}`);
			}
		}
	}
	return { checks, failures };
}
