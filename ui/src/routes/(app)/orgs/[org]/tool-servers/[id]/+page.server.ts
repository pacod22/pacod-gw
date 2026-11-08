import { error } from '@sveltejs/kit';
import { canWrite } from '#lib/access.ts';
import { done, failed, formText, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

const ids = (event: KitEvent) => ({ org: event.params.org ?? '', id: event.params.id ?? '' });

export const load: PageServerLoad = async (event) => {
	const path = { path: ids(event) };
	const [server, tools, credentials] = await Promise.all([
		event.locals.api.GET('/orgs/{org}/tool-servers/{id}', { params: path }),
		event.locals.api.GET('/orgs/{org}/tool-servers/{id}/tools', { params: path }),
		event.locals.api.GET('/orgs/{org}/tool-servers/{id}/credentials', { params: path })
	]);
	if (!server.data)
		error(server.response.status === 404 ? 404 : 502, 'The tool server could not be read.');
	const principal = event.locals.principal;
	return {
		server: server.data,
		tools: tools.data ?? null,
		credentials: credentials.data ?? [],
		mayManage: principal !== null && canWrite(principal, 'organisation', { org: ids(event).org })
	};
};

export const actions = {
	/** Asks the server for its tools now. */
	probe: async (event: KitEvent) => {
		const result = await event.locals.api.POST('/orgs/{org}/tool-servers/{id}/probe', {
			params: { path: ids(event) }
		});
		if (!result.response.ok || !result.data) return failed(event, 'probe', result);
		const probe = result.data.probe;
		return probe?.ok
			? done('probe', `The server speaks ${probe.version} and lists ${probe.tools} tools.`)
			: done('probe', `The probe failed: ${probe?.problem ?? 'no answer'}`, { tone: 'warning' });
	},

	/** Approves a tool as the server lists it now. */
	approve: async (event: KitEvent) => {
		const form = await event.request.formData();
		const tool = formText(form, 'tool');
		const exposed = formText(form, 'exposed_name').trim();
		const result = await event.locals.api.POST(
			'/orgs/{org}/tool-servers/{id}/tools/{tool}/approve',
			{
				params: { path: { ...ids(event), tool } },
				body: { hash: formText(form, 'hash'), exposed_name: exposed || null }
			}
		);
		if (!result.response.ok) return failed(event, 'approve', result, { id: tool });
		return done('approve', `${tool} is approved and served.`, { id: tool });
	},

	/** Stops serving an approved tool. */
	disable: async (event: KitEvent) => {
		const form = await event.request.formData();
		const tool = formText(form, 'tool');
		const result = await event.locals.api.POST(
			'/orgs/{org}/tool-servers/{id}/tools/{tool}/disable',
			{
				params: { path: { ...ids(event), tool } }
			}
		);
		if (!result.response.ok) return failed(event, 'disable', result, { id: tool });
		return done('disable', `${tool} is no longer served.`, { id: tool });
	},

	/** Adds a credential; made current, it retires the one it replaces after a grace period. */
	addCredential: async (event: KitEvent) => {
		const form = await event.request.formData();
		const workspace = formText(form, 'workspace_id').trim();
		const result = await event.locals.api.POST('/orgs/{org}/tool-servers/{id}/credentials', {
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

	/** Makes a credential current, or disables it. */
	credentialState: async (event: KitEvent) => {
		const form = await event.request.formData();
		const cid = formText(form, 'cid');
		const state = formText(form, 'state') === 'current' ? 'current' : 'disabled';
		const result = await event.locals.api.POST(
			'/orgs/{org}/tool-servers/{id}/credentials/{cid}/state',
			{
				params: { path: { ...ids(event), cid } },
				body: { state }
			}
		);
		if (!result.response.ok) return failed(event, 'credentialState', result, { id: cid });
		return done(
			'credentialState',
			state === 'current' ? 'The credential is current.' : 'The credential is disabled.',
			{
				id: cid
			}
		);
	},

	deleteCredential: async (event: KitEvent) => {
		const form = await event.request.formData();
		const cid = formText(form, 'cid');
		const result = await event.locals.api.DELETE(
			'/orgs/{org}/tool-servers/{id}/credentials/{cid}',
			{
				params: { path: { ...ids(event), cid } }
			}
		);
		if (!result.response.ok) return failed(event, 'deleteCredential', result, { id: cid });
		return done('deleteCredential', 'The credential is deleted.', { id: cid });
	}
};
