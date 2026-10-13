import { done, failed, formText, problemOf, type KitEvent } from '#lib/server/resource.ts';
import { identity } from '#lib/server/identity.ts';
import { loadReport, SCOPE } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const admin = event.locals.principal?.platform_admin === true;
	// Today, in UTC, for the gateway-delay figure.
	const today = `${new Date().toISOString().slice(0, 10)}T00:00:00Z`;
	const [overview, usage, delay, alerts, health, status, invitations] = await Promise.all([
		loadReport(event, '/analytics/overview', SCOPE),
		loadReport(event, '/analytics/usage', [...SCOPE, 'bucket'], { bucket: 'hour' }),
		loadReport(event, '/monitoring/delay', [...SCOPE, 'from'], { from: today }),
		// The endpoint answers with the alerts the caller's roles may see.
		event.locals.api.GET('/monitoring/alerts'),
		// Provider health is for platform-level roles; others are refused and see nothing of it.
		event.locals.api.GET('/monitoring/health'),
		// The system status is for platform admins.
		admin ? event.locals.api.GET('/system/status') : null,
		// Invitations to the caller's address, to accept here without the email. With pacod-id
		// they are in the account there.
		identity ? null : event.locals.api.GET('/session/invitations')
	]);
	return {
		overview,
		usage,
		delay: delay.problem ? null : ((delay.data?.summary ?? null) as Record<string, unknown> | null),
		alerts: alerts.response.ok ? (alerts.data ?? []) : null,
		health: health.response.ok ? (health.data ?? []) : null,
		status: status?.data ?? null,
		problem: status && !status.response.ok ? problemOf(event, status) : null,
		invitations: invitations?.data?.items ?? []
	};
};

export const actions = {
	accept: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/session/invitations/{id}/accept', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'accept', result, { id });
		return done('accept', `You joined ${formText(form, 'place')}.`, { id });
	},
	decline: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/session/invitations/{id}/decline', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'decline', result, { id });
		return done('decline', 'The invitation was declined.', { id });
	}
};
