// Runs the shared engine vectors (policy/engine-vectors/), the specification every port of the
// engine must pass.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { parse } from 'yaml';
import { Manifest, ManifestError } from './index.js';
import { generate } from './gen.js';
import { runVectors } from './vectors.js';
import { manifestFromYaml } from './yaml.js';

const dir = join(import.meta.dirname, '../../../policy/engine-vectors');
const manifestText = (name: string) => readFileSync(join(dir, `manifests/${name}.yaml`), 'utf8');
const load = (name: string) => manifestFromYaml(manifestText(name));
const run = (file: string) => runVectors(readFileSync(join(dir, file), 'utf8'), load);

describe('engine vectors', () => {
	it('evaluation', () => {
		const { checks, failures } = run('evaluation.yaml');
		expect(failures).toEqual([]);
		expect(checks).toBeGreaterThan(50);
	});

	it('grants', () => {
		const { checks, failures } = run('grants.yaml');
		expect(failures).toEqual([]);
		expect(checks).toBeGreaterThan(20);
	});

	it('protected', () => {
		const { checks, failures } = run('protected.yaml');
		expect(failures).toEqual([]);
		expect(checks).toBeGreaterThanOrEqual(8);
	});

	it('manifest errors', () => {
		const file = parse(readFileSync(join(dir, 'manifest-errors.yaml'), 'utf8'));
		for (const kase of file.cases) {
			let got = 'ok';
			try {
				Manifest.fromSource(kase.manifest);
			} catch (e) {
				if (!(e instanceof ManifestError)) throw e;
				got = e.code;
			}
			expect(got, kase.name).toBe(kase.expect);
		}
	});

	it('the runner reports failures', () => {
		const file = `
manifests: [acme]
cases:
  - name: wrong on purpose
    subject: { kind: user, bindings: [] }
    checks:
      - { allowed: acme.org.read, at: [platform], expect: true }
      - { can_grant: acme.viewer, at: [platform], expect: ok }
`;
		expect(runVectors(file, load).failures).toHaveLength(2);
	});
});

describe('manifests', () => {
	it('refuses documents of the wrong shape', () => {
		const bad: unknown[] = [
			null,
			[],
			{ product: 'p', grant: { default: 'x.y' }, extra: 1 },
			{ product: 'p', roles: { a: { permissions: ['x.y'] } }, grant: { default: 'x.y' } },
			{
				product: 'p',
				roles: { a: { at: ['platform'], implicit: 'yes' } },
				grant: { default: 'x.y' }
			}
		];
		for (const m of bad) {
			expect(() => Manifest.fromSource(m)).toThrow(
				expect.objectContaining({ code: 'bad_manifest' })
			);
		}
	});

	it('generates a typed module', () => {
		const text = generate(load('acme'), 'acme.yaml');
		expect(text).toContain("export type Role = 'acme.auditor' | 'acme.org_admin'");
		expect(text).toContain("'acme.platform.manage'");
		expect(text).toContain("'acme.members.manage'");
		expect(text).toContain(
			"export type ScopeType = 'platform' | 'organisation' | 'acme.project' | 'acme.workspace';"
		);
	});
});
