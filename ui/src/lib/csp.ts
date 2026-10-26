/**
 * The console's Content-Security-Policy. SvelteKit sends it with every page it renders
 * (`vite.config.ts`) and adds a nonce for its own inline script and the theme script in
 * `app.html`. Pure, so that it is tested without SvelteKit; `hooks.server.ts` applies
 * `allowFormTarget`.
 */
import type { Config } from '@sveltejs/kit/vite';

type Directives = NonNullable<NonNullable<Config['csp']>['directives']>;

export const CSP_DIRECTIVES: Directives = {
	'default-src': ['self'],
	'script-src': ['self'],
	'style-src': ['self'],
	// The server-rendered pages carry style="…" attributes (charts, meters, the brand mark).
	// They are allowed as attributes only: a <style> element still needs 'self'.
	'style-src-attr': ['unsafe-inline'],
	// Fonts and images are files of the build, never data: URLs (vite.config.ts).
	'img-src': ['self'],
	'font-src': ['self'],
	// The browser talks to the console alone; the console's server calls the core.
	'connect-src': ['self'],
	'form-action': ['self'],
	'frame-ancestors': ['none'],
	'base-uri': ['none'],
	'object-src': ['none']
};

/**
 * The policy with `origin` allowed as a form's destination too, or `null` without a policy.
 * With pacod-id, signing out posts to the console, which answers with redirects that end at
 * the issuer, and browsers apply `form-action` to every redirect of a submission. The issuer
 * is known only when the console starts, after the build fixed the policy.
 */
export function allowFormTarget(policy: string | null, origin: string): string | null {
	if (!policy) return null;
	return policy
		.split(';')
		.map((directive) => directive.trim())
		.map((directive) =>
			directive === 'form-action' || directive.startsWith('form-action ')
				? `${directive} ${origin}`
				: directive
		)
		.join('; ');
}
