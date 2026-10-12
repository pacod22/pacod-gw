#!/usr/bin/env node
// pacod-id-authz gen <manifest.yaml> [--out <file>] [--import <module>]
// Writes the TypeScript module for a product manifest (see gen.ts) to --out, or to stdout.

import { readFileSync, writeFileSync } from 'node:fs';
import { argv, exit, stderr, stdout } from 'node:process';
import { generate } from './gen.js';
import { manifestFromYaml } from './yaml.js';

function usage(): never {
	stderr.write('usage: pacod-id-authz gen <manifest.yaml> [--out <file>] [--import <module>]\n');
	exit(2);
}

const args = argv.slice(2);
if (args[0] !== 'gen' || args.length < 2) usage();
const file = args[1];
let out: string | undefined;
let importFrom: string | undefined;
for (let i = 2; i < args.length; i += 2) {
	if (args[i] === '--out') out = args[i + 1];
	else if (args[i] === '--import') importFrom = args[i + 1];
	else usage();
	if (args[i + 1] === undefined) usage();
}

try {
	const text = generate(manifestFromYaml(readFileSync(file, 'utf8')), file, importFrom);
	if (out === undefined) stdout.write(text);
	else writeFileSync(out, text);
} catch (e) {
	stderr.write(`${file}: ${(e as Error).message}\n`);
	exit(1);
}
