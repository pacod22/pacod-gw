import { resource } from '#lib/server/resource.ts';

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
export const actions = users.actions;
