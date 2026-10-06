import { done, failed, formText, resource, type KitEvent } from '#lib/server/resource.ts';

const users = resource({
	path: '/users',
	noun: 'user',
	fields: {
		// The schema cannot say "required when creating": the description does.
		password: {
			required: 'create',
			help: 'At least 12 characters. The user can change it after signing in.'
		}
	}
});

export const load = users.load;

export const actions = {
	...users.actions,

	/** Emails the user a new link to confirm the address. */
	verification: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/users/{id}/verification', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'verification', result, { id });
		return done('verification', `A verification link was sent to ${formText(form, 'email')}.`, {
			id
		});
	},

	/** Emails the user a link to choose a new password. */
	reset: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/users/{id}/password-reset', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'reset', result, { id });
		return done('reset', `A password-reset link was sent to ${formText(form, 'email')}.`, { id });
	},

	/** Marks the address as confirmed without an email, for example when sending fails. */
	confirm: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/users/{id}/email-confirmation', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'confirm', result, { id });
		return done('confirm', `The address ${formText(form, 'email')} is now confirmed.`, { id });
	}
};
