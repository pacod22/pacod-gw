import { error } from '@sveltejs/kit';
import { isUsageEvent, type UsageEvent } from '#lib/insight/usage.ts';
import { loose } from '#lib/server/api.ts';
import { problemOf } from '#lib/server/resource.ts';
import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import { RUN_ID, type MessageRecord } from '#lib/testing.ts';
import type { PageServerLoad } from './$types';

/** How long the page keeps looking for the call in the request log. */
const PATIENCE_S = 45;

export type Found = 'found' | 'pending' | 'missing' | 'not_audited' | 'error';

export const load: PageServerLoad = async (event) => {
	requirePlatformAdmin(event.locals);
	if (!RUN_ID.test(event.params.id)) error(404, 'There is no such message.');
	event.depends('app:message');
	const answer = await tester<MessageRecord>(event.locals, 'GET', `/messages/${event.params.id}`);
	if (answer.problem?.status === 404) error(404, 'There is no such message.');
	const message = answer.data;
	const age = message ? (Date.now() - Date.parse(message.created_at)) / 1000 : 0;
	const late = age > PATIENCE_S;

	let log: UsageEvent | null = null;
	let logState: Found = 'missing';
	let audit: Record<string, unknown> | null = null;
	let auditState: Found = 'missing';
	let problem = answer.problem;

	const rid = message?.request_id ?? '';
	if (rid) {
		const row = await event.locals.api.GET('/requests/{id}', { params: { path: { id: rid } } });
		if (row.response.ok && isUsageEvent(row.data)) {
			log = row.data;
			logState = 'found';
		} else if (row.response.status === 404) {
			logState = late ? 'missing' : 'pending';
		} else {
			logState = 'error';
			problem ??= problemOf(event, row);
		}
	}

	// Reading an audit record is noted in the audit log: it is read only once the request log
	// says that there is one.
	if (log) {
		if (!log.audit_id) {
			auditState = 'not_audited';
		} else {
			const record = await loose(event.locals.api).GET('/requests/{id}/audit', {
				params: { path: { id: rid } }
			});
			if (record.response.ok) {
				audit = record.data as Record<string, unknown>;
				auditState = 'found';
			} else if (record.response.status === 404) {
				const body = record.error as { error?: { details?: { code?: string } } } | undefined;
				auditState =
					body?.error?.details?.code === 'not_audited'
						? 'not_audited'
						: late
							? 'missing'
							: 'pending';
			} else {
				auditState = 'error';
			}
		}
	}

	return { message, problem, log, logState, audit, auditState };
};
