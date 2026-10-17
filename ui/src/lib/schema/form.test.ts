import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
	buildFields,
	fieldPaths,
	humanise,
	initialValue,
	isOptionalGroup,
	overlay,
	pickFields,
	readForm,
	readPicked,
	resolveSchema,
	slugify,
	toDateTimeInput,
	toRfc3339,
	type Field,
	type SchemaSource
} from './form';
import { describeObject, describeResource, onceOnly, scopeOfPath, type OpenApiDoc } from './spec';

// The real document: the tests fail when the API changes in a way the kit cannot render.
const doc = JSON.parse(
	readFileSync(new URL('../api/openapi.json', import.meta.url), 'utf8')
) as OpenApiDoc;

const ref = (name: string) => ({ $ref: `#/components/schemas/${name}` });
const byPath = (fields: Field[], path: string): Field => {
	const all: Field[] = [];
	const walk = (list: Field[]) => {
		for (const f of list) {
			all.push(f);
			walk(f.fields ?? []);
			for (const v of f.variants ?? []) walk(v.fields);
		}
	};
	walk(fields);
	const found = all.find((f) => f.path === path);
	if (!found) throw new Error(`no field ${path}`);
	return found;
};

function form(entries: Record<string, string | string[]>): FormData {
	const data = new FormData();
	for (const [name, value] of Object.entries(entries)) {
		for (const v of Array.isArray(value) ? value : [value]) data.append(name, v);
	}
	return data;
}

describe('humanise', () => {
	it('turns property names into labels and keeps acronyms', () => {
		expect(humanise('display_name')).toBe('Display name');
		expect(humanise('base_url')).toBe('Base URL');
		expect(humanise('kek_id')).toBe('KEK ID');
		expect(humanise('default_timeout_seconds')).toBe('Default timeout seconds');
	});
});

describe('resolveSchema', () => {
	const source: SchemaSource = {
		components: {
			schemas: {
				Colour: { type: 'string', enum: ['red', 'blue'], description: 'A colour.' }
			}
		}
	};

	it('follows a reference and lets sibling keywords win', () => {
		const { schema, nullable } = resolveSchema(source, {
			$ref: '#/components/schemas/Colour',
			description: 'The colour of the thing.',
			default: 'red'
		});
		expect(schema.enum).toEqual(['red', 'blue']);
		expect(schema.description).toBe('The colour of the thing.');
		expect(schema.default).toBe('red');
		expect(nullable).toBe(false);
	});

	it('unwraps "X or null" in both spellings', () => {
		const viaAnyOf = resolveSchema(source, {
			anyOf: [{ $ref: '#/components/schemas/Colour' }, { type: 'null' }]
		});
		expect(viaAnyOf.nullable).toBe(true);
		expect(viaAnyOf.schema.enum).toEqual(['red', 'blue']);

		const viaType = resolveSchema(source, { type: ['string', 'null'], format: 'date-time' });
		expect(viaType.nullable).toBe(true);
		expect(viaType.schema.type).toBe('string');
	});
});

describe('buildFields on the schemas of the management API', () => {
	it('renders a provider: strings, enums with descriptions, lists, numbers, nullable fields', () => {
		const fields = buildFields(doc, ref('Provider'));
		expect(fields.map((f) => [f.key, f.kind])).toEqual([
			['name', 'text'],
			['display_name', 'text'],
			['family', 'enum'],
			['base_url', 'text'],
			['proxy_base_url', 'text'],
			['auth', 'enum'],
			['managed_api', 'enum'],
			['proxy_paths', 'list'],
			['default_timeout_seconds', 'number'],
			['quirks', 'group'],
			['holidays', 'text'],
			['platform', 'enum'],
			['status', 'enum']
		]);

		const name = byPath(fields, 'name');
		expect(name.required).toBe(true);
		expect(name.label).toBe('Name');
		expect(name.help).toContain('Slug');

		// A oneOf that mixes `enum` and `const` members is one list of options.
		const family = byPath(fields, 'family');
		expect(family.required).toBe(true);
		expect(family.options?.map((o) => o.value)).toEqual([
			'anthropic',
			'openai',
			'gemini',
			'ollama'
		]);
		expect(family.options?.find((o) => o.value === 'openai')?.help).toContain('OpenAI-compatible');
		// Gemini's own API or its compatible endpoint, in managed mode.
		expect(byPath(fields, 'managed_api').options?.map((o) => o.value)).toEqual([
			'openai_compatible',
			'native'
		]);

		const auth = byPath(fields, 'auth');
		expect(auth.nullable).toBe(true);
		expect(auth.required).toBe(false);
		expect(auth.help).toBe("Defaults to the family's scheme.");

		expect(byPath(fields, 'proxy_base_url').nullable).toBe(true);
		const timeout = byPath(fields, 'default_timeout_seconds');
		expect(timeout).toMatchObject({ integer: true, min: 0, default: 180 });
		expect(byPath(fields, 'status').default).toBe('active');
	});

	it('renders nested objects as groups', () => {
		const fields = buildFields(doc, ref('Workspace'));
		const settings = byPath(fields, 'settings');
		expect(settings.kind).toBe('group');
		expect(settings.fields?.map((f) => f.path)).toEqual([
			'settings.unsupported_params',
			'settings.provider_tools',
			'settings.on_spool_full',
			'settings.audit',
			'settings.routing_scheme',
			'settings.usage_retention_days',
			'settings.tag_keys',
			'settings.plugin_third_parties',
			'settings.locked'
		]);
		const unsupported = byPath(fields, 'settings.unsupported_params');
		expect(unsupported.kind).toBe('enum');
		expect(unsupported.nullable).toBe(true);
		expect(unsupported.options?.map((o) => o.value)).toEqual(['drop', 'reject']);
		expect(byPath(fields, 'tag_keys').kind).toBe('list');
	});

	it('renders arrays of enums as choices and date-times as such', () => {
		const fields = buildFields(doc, ref('KeyInput'));
		const modes = byPath(fields, 'allowed_modes');
		expect(modes.kind).toBe('choices');
		expect(modes.options?.map((o) => o.value)).toEqual(['proxy', 'managed']);
		expect(modes.default).toEqual(['proxy', 'managed']);
		expect(byPath(fields, 'require_template').kind).toBe('boolean');
		expect(byPath(fields, 'expires_at')).toMatchObject({
			kind: 'text',
			format: 'date-time',
			nullable: true
		});
		// Everything a key input has is also returned: nothing is write-only.
		expect(fields.some((f) => f.kind === 'secret')).toBe(false);
	});

	it('renders tagged unions with one set of fields per variant', () => {
		const fields = buildFields(doc, ref('RoleBinding'));
		const scope = byPath(fields, 'scope');
		expect(scope.kind).toBe('union');
		expect(scope.discriminator).toBe('type');
		expect(scope.variants?.map((v) => v.value)).toEqual(['platform', 'organisation', 'workspace']);
		expect(scope.variants?.[0].fields).toEqual([]);
		expect(scope.variants?.[1].fields.map((f) => [f.path, f.kind, f.required])).toEqual([
			['scope.id', 'text', true]
		]);

		const role = byPath(fields, 'role');
		expect(role.kind).toBe('enum');
		expect(role.options).toHaveLength(5);
		expect(role.options?.[0]).toMatchObject({ value: 'platform_admin', help: 'Everything.' });
	});

	it('makes the strings the document marks writeOnly secrets, and no others', () => {
		const credential = buildFields(doc, ref('CredentialInput'));
		expect(byPath(credential, 'secret')).toMatchObject({ kind: 'secret', required: true });
		expect(byPath(credential, 'make_current').kind).toBe('boolean');
		expect(byPath(credential, 'grace_hours')).toMatchObject({ kind: 'number', nullable: true });
		expect(byPath(credential, 'label').kind).toBe('text');
		expect(byPath(credential, 'scope').kind).toBe('union');

		const user = buildFields(doc, ref('UserInput'));
		expect(byPath(user, 'password').kind).toBe('secret');
		expect(byPath(user, 'email').kind).toBe('text');

		// Part of the input and never returned, yet no secret: a note on a template version.
		const template = buildFields(doc, ref('TemplateInput'));
		expect(byPath(template, 'comment').kind).toBe('text');
	});

	it('honours writeOnly and the password format', () => {
		const fields = buildFields(
			{},
			{
				type: 'object',
				properties: {
					token: { type: 'string', writeOnly: true },
					pin: { type: 'string', format: 'password' },
					id: { type: 'string', readOnly: true }
				}
			}
		);
		expect(fields.map((f) => [f.key, f.kind])).toEqual([
			['token', 'secret'],
			['pin', 'secret']
		]);
	});

	it('applies overrides: hidden, required on create, options, per-variant', () => {
		const overrides = {
			'settings.locked': { hidden: true },
			name: { label: 'Slug' }
		};
		const workspace = buildFields(doc, ref('Workspace'), { overrides });
		expect(fieldPaths(workspace)).not.toContain('settings.locked');
		expect(byPath(workspace, 'name').label).toBe('Slug');

		const user = { password: { required: 'create' as const } };
		expect(
			byPath(buildFields(doc, ref('UserInput'), { overrides: user, mode: 'create' }), 'password')
				.required
		).toBe(true);
		expect(
			byPath(buildFields(doc, ref('UserInput'), { overrides: user, mode: 'replace' }), 'password')
				.required
		).toBe(false);

		const binding = buildFields(doc, ref('RoleBinding'), {
			overrides: {
				principal: { options: [{ value: 'usr_1', label: 'a@example.test', help: '' }] },
				'scope.id@organisation': {
					label: 'Organisation',
					options: [{ value: 'org_1', label: 'Acme', help: '' }]
				}
			}
		});
		expect(byPath(binding, 'principal')).toMatchObject({ kind: 'enum', required: true });
		const scope = byPath(binding, 'scope');
		expect(scope.variants?.[1].fields[0]).toMatchObject({ kind: 'enum', label: 'Organisation' });
		// The workspace variant has a field with the same path; it is left as it was.
		expect(scope.variants?.[2].fields[0]).toMatchObject({ kind: 'text', label: 'ID' });
	});

	it('builds a form for every request body of the API without falling back to JSON', () => {
		const json: string[] = [];
		for (const [path, operations] of Object.entries(doc.paths)) {
			for (const [method, operation] of Object.entries(operations)) {
				const schema = operation.requestBody?.content?.['application/json']?.schema;
				if (schema === undefined) continue;
				const fields = buildFields(doc, schema);
				expect(fields.length, `${method} ${path}`).toBeGreaterThan(0);
				const walk = (list: Field[]) => {
					for (const f of list) {
						if (f.kind === 'json') json.push(`${method} ${path}: ${f.path}`);
						walk(f.fields ?? []);
						for (const v of f.variants ?? []) walk(v.fields);
					}
				};
				walk(fields);
			}
		}
		// The exceptions: price variants, a map of rate objects under free names, and the
		// playground's request, which is the body of another API and has no schema here.
		expect([...json].sort()).toEqual(
			[
				'put /models/{id}/price: variants',
				'post /orgs/{org}/workspaces/{ws}/playground/transform: request',
				'post /orgs/{org}/workspaces/{ws}/playground/chat: request',
				// Lists of objects: the sources of the price job, imported prices, an exported configuration.
				'put /price-job: sources',
				'post /prices/import: prices',
				// A map of holiday calendars under free names.
				'put /settings: holiday_calendars',
				'post /system/config-import: objects',
				// Lists of objects with editors of their own: a template's pool, a scheme's rules.
				'post /orgs/{org}/workspaces/{ws}/templates: spec.models',
				'put /orgs/{org}/workspaces/{ws}/templates/{id}: spec.models',
				'post /orgs/{org}/routing-schemes: rules',
				'put /orgs/{org}/routing-schemes/{id}: rules',
				// A price's long-context tiers and off-peak windows: lists of objects.
				'put /models/{id}/price: tiers',
				'put /models/{id}/price: windows',
				// A plugin binding's settings: each plugin has its own (GET /plugins).
				'post /orgs/{org}/plugin-bindings: settings',
				'put /orgs/{org}/plugin-bindings/{id}: settings',
				'post /orgs/{org}/workspaces/{ws}/plugin-bindings: settings',
				'put /orgs/{org}/workspaces/{ws}/plugin-bindings/{id}: settings',
				// Members a provider adds to every request: free JSON by design.
				'post /providers: quirks.request_fields',
				'put /providers/{id}: quirks.request_fields'
			].sort()
		);
	});
});

describe('schemas of the second stage', () => {
	it('reads the body of a replace: the input and the version as one object', () => {
		const put = doc.paths['/providers/{id}'].put.requestBody?.content?.['application/json']?.schema;
		const fields = buildFields(doc, put!);
		expect(fields.map((f) => f.key)).toContain('name');
		expect(fields.map((f) => f.key)).toContain('version');
		expect(byPath(fields, 'name').required).toBe(true);
		// The kit hides the version: it travels with the form, not in it.
		const hidden = buildFields(doc, put!, { overrides: { version: { hidden: true } } });
		expect(hidden.map((f) => f.key)).not.toContain('version');
	});

	it('renders maps of numbers, read back as numbers', () => {
		const fields = buildFields(doc, ref('GlobalSettings'));
		const rates = byPath(fields, 'exchange_rates');
		expect(rates).toMatchObject({ kind: 'map', numeric: true });
		const form = new FormData();
		form.set('exchange_rates', 'EUR: 1.08\nGBP = 1.27');
		expect(readForm([rates], form)).toEqual({ exchange_rates: { EUR: 1.08, GBP: 1.27 } });
	});

	it('renders maps of strings and lists of numbers', () => {
		const fields = buildFields(doc, ref('LimitSpec'));
		expect(byPath(fields, 'filter.tags').kind).toBe('map');
		expect(byPath(fields, 'alert_thresholds')).toMatchObject({
			kind: 'list',
			numeric: true,
			integer: true
		});
		expect(byPath(fields, 'amount')).toMatchObject({
			kind: 'number',
			integer: false,
			required: true
		});

		const body = readForm(
			fields,
			form({
				name: 'daily',
				subject: 'workspace',
				metric: 'tokens',
				window: 'day',
				amount: '100000',
				enforcement: 'request',
				'filter.models': 'a/b',
				'filter.templates': '',
				'filter.tags': 'purpose: planner\nsite=berlin\nnot a pair',
				alert_thresholds: '50, 80 100',
				status: 'active'
			})
		);
		expect(body).toMatchObject({
			amount: 100000,
			filter: { models: ['a/b'], templates: [], tags: { purpose: 'planner', site: 'berlin' } },
			alert_thresholds: [50, 80, 100]
		});
	});

	it('turns a list into choices when the page offers the values', () => {
		const fields = buildFields(doc, ref('KeyInput'), {
			overrides: { allowed_models: { options: [{ value: 'a/b', label: 'a/b', help: '' }] } }
		});
		expect(byPath(fields, 'allowed_models')).toMatchObject({ kind: 'choices' });
		expect(byPath(fields, 'max_concurrency')).toMatchObject({ kind: 'number', nullable: true });
		expect(readForm(fields, form({ name: 'k', allowed_models: ['a/b'] })).allowed_models).toEqual([
			'a/b'
		]);
	});

	it('renders a model with its capability profile as a group', () => {
		const fields = buildFields(doc, ref('ModelInput'));
		const capabilities = byPath(fields, 'capabilities');
		expect(capabilities).toMatchObject({ kind: 'group', nullable: true });
		expect(byPath(fields, 'capabilities.temperature').options?.map((o) => o.value)).toEqual([
			'accepted',
			'rejected',
			'default_only'
		]);
		expect(byPath(fields, 'capabilities.reasoning_efforts').kind).toBe('list');
		expect(byPath(fields, 'capabilities.vision').kind).toBe('boolean');
		// In the input but not in the view, and yet no secret: it is not marked writeOnly.
		expect(byPath(fields, 'capability_preset').kind).toBe('text');
	});
});

describe('settings that can be left unset', () => {
	const fields = buildFields(doc, ref('Workspace'));

	it('renders audit mode as an optional group and provider tools as a list', () => {
		const audit = byPath(fields, 'settings.audit');
		expect(audit).toMatchObject({ kind: 'group', nullable: true, required: false });
		expect(isOptionalGroup(audit)).toBe(true);
		expect(byPath(fields, 'settings.audit.mode').options?.map((o) => o.value)).toEqual([
			'off',
			'on',
			'on_request'
		]);
		expect(audit.fields?.map((f) => f.key)).toEqual([
			'mode',
			'until',
			'sample',
			'retention_days',
			'attachments',
			'guarantee',
			'rule'
		]);
		expect(byPath(fields, 'settings.provider_tools')).toMatchObject({
			kind: 'list',
			nullable: true
		});
		// The settings object itself is always there.
		expect(isOptionalGroup(byPath(fields, 'settings'))).toBe(false);
	});

	it('sends null for what is not set, so that it is inherited', () => {
		const unset = readForm(
			fields,
			form({ name: 'ws', 'settings.audit.mode': 'on', 'settings.provider_tools': '' })
		);
		expect(unset.settings).toMatchObject({ audit: null, provider_tools: null });

		const set = readForm(
			fields,
			form({
				name: 'ws',
				'settings.audit.__set': 'on',
				'settings.audit.mode': 'on_request',
				'settings.audit.sample': '0.5',
				'settings.audit.retention_days': '30',
				'settings.audit.attachments': 'reference',
				'settings.audit.guarantee': 'best_effort',
				'settings.provider_tools': 'web_search\ncode_execution'
			})
		);
		expect(set.settings).toMatchObject({
			audit: {
				mode: 'on_request',
				sample: 0.5,
				retention_days: 30,
				attachments: 'reference',
				until: null,
				rule: null
			},
			provider_tools: ['web_search', 'code_execution']
		});
	});

	it('has the installation-wide audit settings on the platform settings', () => {
		const global = buildFields(doc, ref('GlobalSettings'));
		expect(byPath(global, 'audit_rule')).toMatchObject({ kind: 'enum', nullable: true });
		expect(byPath(global, 'audit_rule').options?.map((o) => o.value)).toEqual(['forbid', 'force']);
		expect(byPath(global, 'audit_max_body_bytes')).toMatchObject({ kind: 'number', integer: true });
	});

	it('keeps the audit switch of a template', () => {
		const template = buildFields(doc, ref('TemplateSpec'));
		expect(byPath(template, 'audit').options?.map((o) => o.value)).toEqual([
			'on',
			'off',
			'inherit'
		]);
	});
});

describe('readForm', () => {
	it('builds the body of a provider and leaves empty optional controls out', () => {
		const fields = buildFields(doc, ref('Provider'));
		const body = readForm(
			fields,
			form({
				name: ' openai ',
				display_name: '',
				family: 'openai',
				base_url: 'https://api.openai.com/v1',
				proxy_base_url: '',
				auth: '',
				proxy_paths: '/v1/chat\r\n\n /v1/embeddings ',
				default_timeout_seconds: '90',
				status: 'active'
			})
		);
		const { quirks, ...rest } = body;
		expect(rest).toEqual({
			name: 'openai',
			family: 'openai',
			base_url: 'https://api.openai.com/v1',
			// Nullable and empty: null, so that a replace clears the value.
			proxy_base_url: null,
			auth: null,
			proxy_paths: ['/v1/chat', '/v1/embeddings'],
			default_timeout_seconds: 90,
			holidays: null,
			status: 'active'
		});
		// A nested object is always sent; its boxes were not ticked.
		expect(quirks).toMatchObject({ stream_usage: false, cache_control: false, drop_fields: [] });
	});

	it('sends an empty required string so that the API names the problem', () => {
		const fields = buildFields(doc, ref('Organisation'));
		expect(readForm(fields, form({ name: '' }))).toMatchObject({ name: '' });
	});

	it('reads booleans, choices and dates', () => {
		const fields = buildFields(doc, ref('KeyInput'));
		const body = readForm(
			fields,
			form({
				name: 'chat',
				allowed_modes: ['proxy', 'managed'],
				allowed_models: '',
				allowed_templates: 'planner',
				require_template: 'on',
				usage_policy: 'estimate',
				inspect: 'full',
				expires_at: '2027-01-31T08:30'
			})
		);
		expect(body).toEqual({
			name: 'chat',
			allowed_modes: ['proxy', 'managed'],
			allowed_models: [],
			allowed_templates: ['planner'],
			require_template: true,
			usage_policy: 'estimate',
			rate_class: null,
			inspect: 'full',
			max_concurrency: null,
			expires_at: '2027-01-31T08:30:00Z'
		});

		// An unchecked box is not part of a submitted form: it means false.
		const off = readForm(fields, form({ name: 'chat', expires_at: '' }));
		expect(off.require_template).toBe(false);
		expect(off.allowed_modes).toEqual([]);
		expect(off.expires_at).toBeNull();
	});

	it('reads nested objects and tagged unions', () => {
		const workspace = buildFields(doc, ref('Workspace'));
		expect(
			readForm(
				workspace,
				form({
					name: 'prod',
					'settings.unsupported_params': 'reject',
					'settings.locked': '',
					tag_keys: 'purpose\nsite',
					time_zone: 'Europe/Berlin'
				})
			)
		).toMatchObject({
			name: 'prod',
			settings: { unsupported_params: 'reject', locked: [] },
			tag_keys: ['purpose', 'site'],
			time_zone: 'Europe/Berlin'
		});

		const binding = buildFields(doc, ref('RoleBinding'));
		expect(
			readForm(
				binding,
				form({
					principal: 'usr_1',
					role: 'org_admin',
					'scope.type': 'organisation',
					'scope.id': 'org_1'
				})
			)
		).toEqual({
			principal: 'usr_1',
			role: 'org_admin',
			scope: { type: 'organisation', id: 'org_1' }
		});
		// A variant without fields is just its tag.
		expect(
			readForm(
				binding,
				form({ principal: 'usr_1', role: 'viewer', 'scope.type': 'platform', 'scope.id': 'stale' })
			)
		).toMatchObject({ scope: { type: 'platform' } });
	});

	it('sends a secret only when it was filled, and as typed', () => {
		const fields = buildFields(doc, ref('UserInput'));
		const kept = readForm(
			fields,
			form({ email: 'a@example.test', display_name: 'A', password: '', status: 'active' })
		);
		expect(kept).toEqual({ email: 'a@example.test', display_name: 'A', status: 'active' });
		expect('password' in kept).toBe(false);

		const changed = readForm(
			fields,
			form({ email: 'a@example.test', password: '  spaces stay  ' })
		);
		expect(changed.password).toBe('  spaces stay  ');
	});

	it('reads numbers, leaves empty ones to the default and passes nonsense on', () => {
		const fields = buildFields(doc, ref('GlobalSettings'));
		expect(readForm(fields, form({ session_hours: '24' })).session_hours).toBe(24);
		expect('session_hours' in readForm(fields, form({ session_hours: '' }))).toBe(false);
		expect(readForm(fields, form({ session_hours: 'soon' })).session_hours).toBe('soon');

		const rotate = buildFields(doc, ref('RotateInput'));
		expect(readForm(rotate, form({ grace_hours: '' }))).toEqual({ grace_hours: null });
		expect(readForm(rotate, form({ grace_hours: '0' }))).toEqual({ grace_hours: 0 });
	});
});

describe('values shown in a form', () => {
	it('starts from the object, or from the schema default for a new one', () => {
		const fields = buildFields(doc, ref('Workspace'));
		const zone = byPath(fields, 'time_zone');
		expect(initialValue(zone, undefined)).toBe('UTC');
		expect(initialValue(zone, { time_zone: 'Europe/Berlin' })).toBe('Europe/Berlin');
		const nested = byPath(fields, 'settings.unsupported_params');
		expect(initialValue(nested, { settings: { unsupported_params: 'reject' } })).toBe('reject');
		expect(initialValue(nested, { settings: {} })).toBeUndefined();
	});

	it('never shows a secret', () => {
		const fields = buildFields(doc, ref('CredentialInput'));
		expect(initialValue(byPath(fields, 'secret'), { secret: 'sk-live' })).toBe('');
	});

	it('converts times to and from the UTC form of a datetime-local input', () => {
		expect(toRfc3339('2026-10-04T19:33')).toBe('2026-10-04T19:33:00Z');
		expect(toRfc3339('2026-10-04T19:33:10')).toBe('2026-10-04T19:33:10Z');
		expect(toRfc3339('2026-10-04T19:33:10+02:00')).toBe('2026-10-04T19:33:10+02:00');
		expect(toDateTimeInput('2026-10-04T19:33:05.399247Z')).toBe('2026-10-04T19:33');
		expect(toDateTimeInput(null)).toBe('');
		expect(toDateTimeInput('not a time')).toBe('');
	});
});

describe('describeResource', () => {
	it('finds the operations and schemas of a collection', () => {
		const keys = describeResource(doc, '/orgs/{org}/workspaces/{ws}/keys');
		expect(keys).toMatchObject({
			item: '/orgs/{org}/workspaces/{ws}/keys/{id}',
			idParam: 'id',
			params: ['org', 'ws'],
			listQuery: ['cursor', 'limit', 'sort', 'order', 'q', 'status'],
			remove: true
		});
		expect(keys.create?.input).toEqual(ref('KeyInput'));
		// A replace takes the input plus the version the caller last read.
		expect(Object.keys(resolveSchema(doc, keys.replace?.input).schema.properties ?? {})).toEqual(
			expect.arrayContaining(['name', 'allowed_modes', 'version'])
		);
		// The creation answer carries the secret, the regular view does not.
		expect(onceOnly(doc, keys.create?.output, keys.view)).toEqual(['secret']);

		const orgs = describeResource(doc, '/orgs');
		expect(orgs).toMatchObject({ item: '/orgs/{org}', idParam: 'org', params: [] });
		expect(onceOnly(doc, orgs.create?.output, orgs.view)).toEqual([]);
	});

	it('copes with resources that lack operations', () => {
		const credentials = describeResource(doc, '/providers/{id}/credentials');
		expect(credentials).toMatchObject({
			item: '/providers/{id}/credentials/{cid}',
			idParam: 'cid',
			replace: null,
			remove: true,
			listQuery: ['all', 'cursor', 'limit']
		});
		expect(credentials.view).toEqual(ref('Item5'));

		const audit = describeResource(doc, '/audit-log');
		expect(audit).toMatchObject({ item: null, create: null, replace: null, remove: false });
		expect(audit.listQuery).toContain('organisation_id');

		const bindings = describeResource(doc, '/role-bindings');
		expect(bindings.replace).toBeNull();
		expect(bindings.create).not.toBeNull();

		expect(() => describeResource(doc, '/nothing')).toThrow(/no path/);
	});

	it('describes single objects and the scope a path lies in', () => {
		expect(describeObject(doc, '/settings').replace?.input).toBeDefined();
		expect(describeObject(doc, '/orgs/{org}/workspaces/{ws}').params).toEqual(['org', 'ws']);

		expect(scopeOfPath('/providers')).toBe('platform');
		expect(scopeOfPath('/providers/{id}/credentials')).toBe('platform');
		expect(scopeOfPath('/orgs')).toBe('platform');
		expect(scopeOfPath('/orgs/{org}')).toBe('platform');
		expect(scopeOfPath('/orgs/{org}/workspaces')).toBe('organisation');
		expect(scopeOfPath('/orgs/{org}/workspaces/{ws}')).toBe('organisation');
		expect(scopeOfPath('/orgs/{org}/workspaces/{ws}/keys')).toBe('workspace');
		expect(scopeOfPath('/orgs/{org}/workspaces/{ws}/keys/{id}')).toBe('workspace');
	});
});

describe('forms that change part of an object', () => {
	const workspace = buildFields(doc, ref('Workspace'), { mode: 'replace' });
	const current = {
		id: 'ws_1',
		version: 3,
		name: 'billing',
		display_name: 'Billing',
		status: 'active',
		time_zone: 'Europe/Vienna',
		tag_keys: ['purpose'],
		settings: { provider_tools: ['web_search'], audit: { mode: 'full', sample: 0.5 } }
	};

	it('picks nested fields by path', () => {
		const picked = pickFields(workspace, ['display_name', 'settings.provider_tools', 'nope']);
		expect(picked.map((f) => f.path)).toEqual(['display_name', 'settings.provider_tools']);
	});

	it('changes only the section that was submitted', () => {
		const general = pickFields(workspace, ['display_name', 'time_zone', 'tag_keys']);
		const values = readPicked(
			general,
			form({ display_name: 'Billing EU', time_zone: 'UTC', tag_keys: 'purpose\nsite' })
		);
		const body = overlay(current, values);
		expect(body).toMatchObject({
			name: 'billing',
			display_name: 'Billing EU',
			time_zone: 'UTC',
			tag_keys: ['purpose', 'site'],
			settings: { provider_tools: ['web_search'], audit: { mode: 'full', sample: 0.5 } }
		});
		// The object it started from is left as it was.
		expect(current.display_name).toBe('Billing');
	});

	it('clears an inherited setting and removes a value left empty', () => {
		const defaults = pickFields(workspace, ['settings.provider_tools', 'display_name']);
		const body = overlay(current, readPicked(defaults, form({ 'settings.provider_tools': '' })));
		expect((body.settings as Record<string, unknown>).provider_tools).toBeNull();
		expect('display_name' in body).toBe(false);
	});
});

describe('slugify', () => {
	it('makes a name the API accepts from a display name', () => {
		expect(slugify('Acme Corp.')).toBe('acme-corp');
		expect(slugify('  Zürich – Team 2 ')).toBe('zurich-team-2');
		expect(slugify('---')).toBe('');
		expect(slugify('x'.repeat(70))).toHaveLength(63);
	});
});
