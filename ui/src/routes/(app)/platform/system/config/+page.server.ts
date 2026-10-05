import { fail } from '@sveltejs/kit';
import { localProblem } from '#lib/api/errors.ts';
import { loose } from '#lib/server/api.ts';
import { done, failed, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = ({ locals }) => ({
	admin: locals.principal?.platform_admin === true
});

/** The document from the uploaded file or, without one, from the text field. */
async function documentOf(form: FormData): Promise<unknown> {
	const file = form.get('file');
	const text =
		file instanceof File && file.size > 0 ? await file.text() : String(form.get('document') ?? '');
	if (!text.trim()) throw new Error('Choose the exported file, or paste its content.');
	try {
		return JSON.parse(text);
	} catch {
		throw new Error('The document is not valid JSON. Use a file that the export produced.');
	}
}

async function send(event: KitEvent, action: 'check' | 'apply') {
	let document: unknown;
	try {
		document = await documentOf(await event.request.formData());
	} catch (e) {
		return fail(400, {
			action,
			ok: false,
			problem: localProblem(e instanceof Error ? e.message : 'The document could not be read.')
		});
	}
	const result = await loose(event.locals.api).POST('/system/config-import', {
		params: { query: action === 'check' ? { dry_run: 'true' } : {} },
		body: document
	});
	if (!result.response.ok) return failed(event, action, result);
	const data = result.data as { applied?: boolean; refused?: unknown[] };
	return done(
		action,
		action === 'check'
			? ''
			: data.applied
				? 'The configuration was imported.'
				: 'Nothing was imported: the document holds objects the gateway refuses.',
		{ data: result.data, tone: action === 'apply' && !data.applied ? 'warning' : 'success' }
	);
}

export const actions = {
	/** A dry run: what the import would create, update, leave and refuse. */
	check: (event: KitEvent) => send(event, 'check'),
	apply: (event: KitEvent) => send(event, 'apply')
};
