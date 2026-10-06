import { fail, redirect } from '@sveltejs/kit';
import { localProblem } from '#lib/api/errors.ts';
import { compose, restoreDraft, type Draft } from '#lib/message.ts';
import type { KitEvent } from '#lib/server/resource.ts';
import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import { RUN_ID, type MessageRecord, type MessageSummary } from '#lib/testing.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	requirePlatformAdmin(event.locals);
	const from = event.url.searchParams.get('from') ?? '';
	const [orgs, recent, earlier] = await Promise.all([
		event.locals.api.GET('/orgs', { params: { query: { limit: 500 } } }),
		tester<{ items: MessageSummary[] }>(event.locals, 'GET', '/messages'),
		RUN_ID.test(from)
			? tester<MessageRecord>(event.locals, 'GET', `/messages/${from}`)
			: Promise.resolve(null)
	]);
	return {
		orgs: (orgs.data?.items ?? []).map((o) => ({ id: o.id, name: o.display_name || o.name })),
		recent: recent.data?.items.slice(0, 25) ?? [],
		problem: recent.problem,
		// "Edit and send again" starts from the draft of an earlier message.
		draft: earlier?.data?.draft ? restoreDraft(earlier.data.draft) : null
	};
};

function failure(problem: ReturnType<typeof localProblem>) {
	// The pasted key is never sent back to the page.
	return fail(problem.status >= 400 && problem.status <= 599 ? problem.status : 502, {
		action: 'send',
		ok: false,
		problem
	});
}

export const actions = {
	/** Sends the message through the tester, then opens its trace. */
	send: async (event: KitEvent) => {
		requirePlatformAdmin(event.locals);
		const form = await event.request.formData();
		let draft: Draft;
		try {
			draft = restoreDraft(JSON.parse(String(form.get('draft') ?? '')));
		} catch {
			return failure(localProblem('The form could not be read; reload the page.'));
		}
		draft.keyKind = form.get('key_kind') === 'own' ? 'own' : 'temporary';
		const ownKey = String(form.get('own_key') ?? '').trim();
		if (!draft.org || !draft.ws) return failure(localProblem('Choose the workspace to send as.'));
		if (draft.keyKind === 'own' && !ownKey)
			return failure(localProblem('Paste the API key to send the message with.'));

		// The same function the page used builds what is sent, so that what was shown is sent.
		const composed = compose(draft);
		if (composed.problems.length) return failure(localProblem(composed.problems.join(' ')));

		const { keyKind, ...saved } = draft;
		void keyKind;
		const result = await tester<MessageRecord>(
			event.locals,
			'POST',
			'/messages',
			{
				json: {
					org: draft.org,
					ws: draft.ws,
					key: draft.keyKind === 'own' ? { kind: 'own', secret: ownKey } : { kind: 'temporary' },
					method: composed.method,
					path: composed.path,
					headers: composed.headers,
					auth_header: composed.authHeader,
					body: composed.body,
					timeout_s: draft.timeoutS,
					draft: saved
				}
			},
			(draft.timeoutS + 30) * 1000
		);
		if (result.problem) return failure(result.problem);
		redirect(303, `/platform/testing/message/${result.data.id}`);
	}
};
