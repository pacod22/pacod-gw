import { readForm } from '#lib/schema/form.ts';
import { loose } from '#lib/server/api.ts';
import { grantedModels } from '#lib/server/options.ts';
import { done, failed, inputFields, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

const TRANSFORM = '/orgs/{org}/workspaces/{ws}/playground/transform';
const CHAT = '/orgs/{org}/workspaces/{ws}/playground/chat';

const fields = () =>
	inputFields(TRANSFORM, 'post', {
		request: {
			label: 'Request body',
			help: 'The body as an application would send it to that interface. `model` names a model or, as `@name`, a template.'
		},
		headers: {
			label: 'Gateway headers',
			help: 'For example `x-gw-template: planner` or `x-gw-end-user: alice`.'
		}
	});

export const load: PageServerLoad = async (event) => {
	const models = await grantedModels(event, event.params.org, event.params.ws);
	const model = models.find((m) => m.state === 'active')?.name ?? 'provider/model';
	return {
		fields: fields(),
		// A body both chat dialects accept, to start from.
		example: {
			interface: 'openai',
			request: {
				model,
				max_tokens: 200,
				messages: [{ role: 'user', content: 'Say hello in one sentence.' }]
			}
		}
	};
};

async function call(event: KitEvent, action: 'transform' | 'chat') {
	const form = await event.request.formData();
	const list = fields();
	const result = await loose(event.locals.api).POST(action === 'transform' ? TRANSFORM : CHAT, {
		params: { path: { org: event.params.org ?? '', ws: event.params.ws ?? '' } },
		body: readForm(list, form)
	});
	if (!result.response.ok) return failed(event, action, result, { fields: list });
	return done(action, '', { data: result.data });
}

export const actions = {
	/** Shows what the gateway would send to the provider. Nothing is called. */
	transform: (event: KitEvent) => call(event, 'transform'),
	/** Makes the call as the workspace. It is counted and billed like any other call. */
	chat: (event: KitEvent) => call(event, 'chat')
};
