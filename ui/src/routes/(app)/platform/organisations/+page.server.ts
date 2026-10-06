import { readForm, type Field, type FieldOverrides } from '#lib/schema/form.ts';
import { loose } from '#lib/server/api.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	resource,
	type KitEvent
} from '#lib/server/resource.ts';

// Creating asks for the names and, if wanted, who administers the organisation; everything
// else is on the organisation's Settings page, which is also where it is deleted.
const FIELDS: FieldOverrides = {
	display_name: {
		label: 'Display name',
		help: 'How the console shows the organisation, for example “Acme Corp.”.',
		required: true,
		order: -1
	},
	name: {
		label: 'Name',
		help: 'Lower-case letters, digits and hyphens; unique in the installation. Filled in from the display name.',
		slugFrom: 'display_name'
	},
	status: { hidden: true },
	settings: { hidden: true }
};

const organisations = resource({ path: '/orgs', noun: 'organisation', fields: FIELDS });

/** Not part of the organisation: invited as its first admin once it exists. */
const ADMIN_EMAIL: Field = {
	kind: 'text',
	key: 'admin_email',
	path: 'admin_email',
	label: 'First organisation admin',
	help: 'Optional. The email address of the person who will run the organisation: they get an invitation.',
	required: false,
	nullable: false,
	format: 'email'
};

export const load = async (event: KitEvent) => {
	const data = await organisations.load(event);
	return {
		...data,
		createFields: data.createFields ? [...data.createFields, ADMIN_EMAIL] : null,
		replaceFields: null,
		canDelete: false
	};
};

export const actions = {
	create: async (event: KitEvent) => {
		const form = await event.request.formData();
		const fields = inputFields('/orgs', 'post', FIELDS);
		const created = await loose(event.locals.api).POST('/orgs', { body: readForm(fields, form) });
		if (!created.response.ok) return failed(event, 'create', created, { fields });
		const org = created.data as { id: string; name: string; display_name?: string };
		const label = org.display_name || org.name;

		const email = formText(form, 'admin_email').trim();
		if (!email) return done('create', `Organisation “${label}” was created.`);
		const invited = await event.locals.api.POST('/orgs/{org}/invitations', {
			params: { path: { org: org.id } },
			body: { email, role: 'org_admin', workspace_id: null }
		});
		if (!invited.response.ok || !invited.data) {
			return done(
				'create',
				`Organisation “${label}” was created, but ${email} could not be invited: ${invited.error?.error?.message ?? 'the gateway refused it'}. Invite them from the organisation’s Members page.`,
				{ tone: 'warning' }
			);
		}
		const token = invited.data.token;
		if (!token) {
			return done(
				'create',
				`Organisation “${label}” was created and ${email} was invited by email.`
			);
		}
		return done('create', `Organisation “${label}” was created.`, {
			secret: {
				title: `Invitation for ${email}`,
				dismiss: 'Done',
				value: `${event.url.origin}/invite?token=${encodeURIComponent(token)}`,
				note: invited.data.email_error
					? `The email could not be sent (${invited.data.email_error}). Send this link to ${email} yourself: it works once and makes them the organisation’s admin.`
					: `This installation sends no email. Send this link to ${email} yourself: it works once and makes them the organisation’s admin.`
			}
		});
	}
};
