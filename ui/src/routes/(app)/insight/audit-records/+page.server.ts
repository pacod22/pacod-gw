import { canReadAuditRecords } from '#lib/access.ts';
import { loose } from '#lib/server/api.ts';
import { done, failed, formText, type KitEvent } from '#lib/server/resource.ts';
import { loadReport, RANGE, SCOPE } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => ({
	report: await loadReport(event, '/audit-records', [
		...RANGE,
		...SCOPE,
		'model',
		'template',
		'key_id',
		'end_user',
		'session_id',
		'trace_id',
		'cursor'
	]),
	cursor: event.url.searchParams.get('cursor'),
	mayDelete: event.locals.principal !== null && canReadAuditRecords(event.locals.principal)
});

export const actions = {
	/** Deletes the audit records of one end user or one session in a workspace. */
	delete: async (event: KitEvent) => {
		const form = await event.request.formData();
		const query: Record<string, string> = {};
		for (const name of ['workspace_id', 'end_user', 'session_id']) {
			const value = formText(form, name).trim();
			if (value) query[name] = value;
		}
		const result = await loose(event.locals.api).DELETE('/audit-records', { params: { query } });
		if (!result.response.ok) return failed(event, 'delete', result);
		const answer = result.data as Record<string, unknown> | undefined;
		const count = Object.values(answer ?? {}).find((v) => typeof v === 'number');
		return done(
			'delete',
			typeof count === 'number'
				? `${count} audit ${count === 1 ? 'record was' : 'records were'} deleted.`
				: 'The audit records were deleted.'
		);
	}
};
