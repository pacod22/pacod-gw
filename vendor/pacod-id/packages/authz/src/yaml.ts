// Reading manifests written in YAML (authz/<product>.yaml).

import { parse } from 'yaml';
import { Manifest, ManifestError } from './manifest.js';

/** Reads and checks a manifest written in YAML. */
export function manifestFromYaml(text: string): Manifest {
	let value: unknown;
	try {
		value = parse(text);
	} catch (e) {
		throw new ManifestError('bad_manifest', `not YAML: ${(e as Error).message}`);
	}
	return Manifest.fromSource(value);
}
