// Writes dist/tokens.css, dist/console.css and dist/tailwind.css (see tokens.js).

import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { consoleCss, readTokens, tailwindCss, tokensCss } from './tokens.js';

const root = join(import.meta.dirname, '..');
const tokens = readTokens(join(root, 'tokens'));
const dist = join(root, 'dist');
mkdirSync(dist, { recursive: true });
writeFileSync(join(dist, 'tokens.css'), tokensCss(tokens));
writeFileSync(join(dist, 'console.css'), consoleCss(tokens));
writeFileSync(join(dist, 'tailwind.css'), tailwindCss());
