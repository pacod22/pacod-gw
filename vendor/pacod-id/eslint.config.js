import prettier from 'eslint-config-prettier';
import path from 'node:path';
import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import { defineConfig, includeIgnoreFile } from 'eslint/config';
import globals from 'globals';
import ts from 'typescript-eslint';

const gitignorePath = path.resolve(import.meta.dirname, '.gitignore');

// One configuration for every workspace (packages/*, ui, fixtures/*). Svelte rules are added
// where Svelte code appears.
export default defineConfig(
	includeIgnoreFile(gitignorePath),
	{ ignores: ['target/', 'python/', 'policy/'] },
	js.configs.recommended,
	ts.configs.recommended,
	svelte.configs.recommended,
	prettier,
	svelte.configs.prettier,
	{
		languageOptions: { globals: { ...globals.browser, ...globals.node } },
		rules: {
			// typescript-eslint advises against no-undef on TypeScript projects.
			'no-undef': 'off'
		}
	},
	{
		// The packages are libraries: their links come from the app using them, which resolves
		// its own routes.
		files: ['packages/**/*.svelte'],
		rules: { 'svelte/no-navigation-without-resolve': 'off' }
	},
	{
		files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
		languageOptions: {
			parserOptions: {
				projectService: true,
				extraFileExtensions: ['.svelte'],
				parser: ts.parser
			}
		}
	}
);
