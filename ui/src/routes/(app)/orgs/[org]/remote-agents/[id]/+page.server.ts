import { error } from '@sveltejs/kit';
import { canWrite } from '#lib/access.ts';
import { done, failed, formText, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

const ids = (event: KitEvent) => ({ org: event.params.org ?? '', id: event.params.id ?? '' });

export const load: PageServerLoad = async (event) => {
	const path = { path: ids(event) };
	const [agent, card, credentials] = await Promise.all([
		event.locals.api.GET('/orgs/{org}/remote-agents/{id}', { params: path }),
		event.locals.api.GET('/orgs/{org}/remote-agents/{id}/card', { params: path }),
		event.locals.api.GET('/orgs/{org}/remote-agents/{id}/credentials', { params: path })
	]);
	if (!agent.data)
		error(agent.response.status === 404 ? 404 : 502, 'The remote agent could not be read.');
	const principal = event.locals.principal;
	return {
		agent: agent.data,
		card: card.data ?? null,
		credentials: credentials.data ?? [],
		mayManage: principal !== null && canWrite(principal, 'organisation', { org: ids(event).org })
	};
};

export const actions = {
	probe: async (event: KitEvent) => {
		const result = await event.locals.api.POST('/orgs/{org}/remote-agents/{id}/probe', {
			params: { path: ids(event) }
		});
		if (!result.response.ok || !result.data) return failed(event, 'probe', result);
		const probe = result.data.probe;
		return probe?.ok
			? done('probe', 'The card was read.')
			: done('probe', `Reading the card failed: ${probe?.problem ?? 'no answer'}`, {
					tone: 'warning'
				});
	},

	approve: async (event: KitEvent) => {
		const form = await event.request.formData();
		const result = await event.locals.api.POST('/orgs/{org}/remote-agents/{id}/card/approve', {
			params: { path: ids(event) },
			body: { hash: formText(form, 'hash') }
		});
		if (!result.response.ok) return failed(event, 'approve', result);
		return done('approve', 'The card is approved: callers reach the agent.');
	},

	addCredential: async (event: KitEvent) => {
		const form = await event.request.formData();
		const workspace = formText(form, 'workspace_id').trim();
		const result = await event.locals.api.POST('/orgs/{org}/remote-agents/{id}/credentials', {
			params: { path: ids(event) },
			body: {
				secret: formText(form, 'secret'),
				label: formText(form, 'label'),
				workspace_id: workspace || null,
				make_current: true,
				grace_hours: null
			}
		});
		if (!result.response.ok) return failed(event, 'addCredential', result);
		return done('addCredential', 'The credential is stored and used from now on.');
	},

	deleteCredential: async (event: KitEvent) => {
		const form = await event.request.formData();
		const cid = formText(form, 'cid');
		const result = await event.locals.api.DELETE(
			'/orgs/{org}/remote-agents/{id}/credentials/{cid}',
			{
				params: { path: { ...ids(event), cid } }
			}
		);
		if (!result.response.ok) return failed(event, 'deleteCredential', result, { id: cid });
		return done('deleteCredential', 'The credential is deleted.', { id: cid });
	}
};
