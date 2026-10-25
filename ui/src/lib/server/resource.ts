/**
 * The resource kit, server side.
 *
 * `resource({ path })` returns the `load` function and the form `actions` of a page that
 * lists, creates, replaces and deletes the objects of one API collection. Everything it needs
 * to know about the resource (which operations exist, the item path, the request and response
 * schemas, the query parameters of the listing) comes from the OpenAPI document.
 *
 * `objectResource({ path })` does the same for a single object that is read and replaced at
 * one path, such as the global settings or a workspace.
 */
import { error, fail, type ActionFailure, type Cookies } from '@sveltejs/kit';
import { canWrite } from '#lib/access.ts';
import { toProblem, type ApiProblem } from '#lib/api/errors.ts';
import type { paths } from '#lib/api/schema.d.ts';
import {
	buildFields,
	fieldPaths,
	readForm,
	type Field,
	type FieldOverrides,
	type FormMode,
	type JsonSchema
} from '#lib/schema/form.ts';
import { describeObject, describeResource, onceOnly, scopeOfPath } from '#lib/schema/spec.ts';
import { loose, type ApiResult } from './api';
import { endSession } from './session';
import { spec } from './spec';

/** What the kit reads from a request event. Load events and action events both fit. */
export interface KitEvent {
	params: Partial<Record<string, string>>;
	url: URL;
	locals: App.Locals;
	cookies: Cookies;
	request: Request;
}

/** A secret the API returns a single time. */
export interface OnceSecret {
	title: string;
	value: string;
	note: string;
	/** The button that closes the dialog; "I have stored it" by default. */
	dismiss?: string;
}

/** What every action of the kit answers. A failure is the same object inside `fail(...)`. */
export interface ActionOutcome {
	/** The name of the action, so that the page shows the result next to the right form. */
	action: string;
	ok: boolean;
	/** The object the action was about. */
	id?: string;
	message?: string;
	/** `warning`: the action was carried out, but the result needs attention. */
	tone?: 'success' | 'warning';
	problem?: ApiProblem;
	secret?: OnceSecret;
	/** What the action found, for the page to show: for example the impact of a change. */
	data?: unknown;
}

export type Outcome = ActionOutcome | ActionFailure<ActionOutcome>;

/** Paths whose GET answers a page of items. */
export type CollectionPath = {
	[P in keyof paths]: paths[P] extends {
		get: { responses: { 200: { content: { 'application/json': { items: unknown[] } } } } };
	}
		? P
		: never;
}[keyof paths];

export type ItemOf<P extends CollectionPath> = paths[P] extends {
	get: { responses: { 200: { content: { 'application/json': { items: (infer T)[] } } } } };
}
	? T
	: never;

/** Paths whose GET answers one object and whose PUT replaces it. */
export type ObjectPath = {
	[P in keyof paths]: paths[P] extends {
		get: { responses: { 200: { content: { 'application/json': object } } } };
		put: object;
	}
		? P
		: never;
}[keyof paths];

export type ObjectOf<P extends ObjectPath> = paths[P] extends {
	get: { responses: { 200: { content: { 'application/json': infer T } } } };
}
	? T
	: never;

export type Overrides =
	FieldOverrides | ((event: KitEvent) => FieldOverrides | Promise<FieldOverrides>);

export interface ResourceOptions<P extends CollectionPath, E extends Record<string, unknown>> {
	/** The collection path as the OpenAPI document spells it. */
	path: P;
	/** What one object is called in messages: "provider", "API key". */
	noun: string;
	/** Items per page. */
	pageSize?: number;
	/** Changes to form fields that the schema cannot express. */
	fields?: Overrides;
	/**
	 * Who may write, when it is not what the path suggests: grants of an organisation, for
	 * example, are written by platform admins.
	 */
	writers?: 'platform' | 'organisation' | 'workspace';
	/** More data for the page, loaded next to the list. */
	extra?: (event: KitEvent) => Promise<E>;
	/**
	 * The name of an object in messages. Default: its `name`, `email`, `label` or `id`. It
	 * may look the name up, for an object that only refers to what it is about.
	 */
	nameOf?: (item: ItemOf<P>, event: KitEvent) => string | Promise<string>;
	/**
	 * Changes the body read from a create or replace form before it is sent: where one control
	 * gives a value that the API wants in two places.
	 */
	prepare?: (body: Record<string, unknown>) => Record<string, unknown>;
}

export interface ResourceData<T> {
	noun: string;
	items: T[];
	/** The cursor of this page; `null` on the first page. */
	cursor: string | null;
	nextCursor: string | null;
	/** Why the list could not be loaded. */
	problem: ApiProblem | null;
	/** The form to create an object; `null` when the API or the caller's role does not allow it. */
	createFields: Field[] | null;
	replaceFields: Field[] | null;
	canDelete: boolean;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function capitalise(text: string): string {
	return text ? text[0].toUpperCase() + text.slice(1) : text;
}

function defaultName(item: unknown): string {
	if (!isRecord(item)) return '';
	for (const key of ['name', 'email', 'label', 'id']) {
		const value = item[key];
		if (typeof value === 'string' && value) return value;
	}
	return '';
}

/** The values of a path's parameters, from the route parameters of the same name. */
export function pathValues(names: string[], event: KitEvent): Record<string, string> {
	const out: Record<string, string> = {};
	for (const name of names) {
		const value = event.params[name];
		if (!value) error(404, 'Not found');
		out[name] = value;
	}
	return out;
}

/**
 * The query parameters an operation declares, taken from the page's URL. Filters, cursors
 * and limits pass through without the page naming them.
 */
export function queryFromUrl(
	declared: string[],
	url: URL,
	defaults: Record<string, string> = {}
): Record<string, string> {
	const out: Record<string, string> = {};
	for (const name of declared) {
		const value = url.searchParams.get(name) ?? defaults[name];
		if (value !== undefined && value !== '') out[name] = value;
	}
	return out;
}

function isForwarded(body: unknown): boolean {
	return isRecord(body) && isRecord(body.error) && typeof body.error.code === 'string';
}

/** Describes a failed call. An ended session sends the user to the login page instead. */
export function problemOf(
	event: KitEvent,
	result: ApiResult,
	fieldNames: readonly string[] = []
): ApiProblem {
	const problem = toProblem(result.response.status, result.error, fieldNames);
	// The core's own 401 means that the session has ended. A 401 that the core merely passes
	// on from the analytics service (which names its errors with `code`) does not.
	if (
		result.response.status === 401 &&
		problem.type === 'unauthorized' &&
		!isForwarded(result.error)
	) {
		endSession(event);
	}
	return problem;
}

/**
 * " Applied on 1 of 3 instances so far." after a change, when several instances run and not
 * all have it yet (concept section 11.2). Empty otherwise.
 */
export function appliedNote(response: Response): string {
	const [applied, of] = (response.headers.get('x-gw-applied') ?? '').split('/').map(Number);
	if (!(of > 1) || !(applied < of)) return '';
	return ` Applied on ${applied} of ${of} instances so far; the others follow within seconds.`;
}

/** The answer of an action that worked. */
export function done(
	action: string,
	message: string,
	more: Pick<ActionOutcome, 'id' | 'secret' | 'data' | 'tone'> = {}
): ActionOutcome {
	return { action, ok: true, message, ...more };
}

/** The answer of an action whose API call failed. */
export function failed(
	event: KitEvent,
	action: string,
	result: ApiResult,
	more: { id?: string; fields?: Field[] } = {}
): ActionFailure<ActionOutcome> {
	const problem = problemOf(event, result, more.fields ? fieldPaths(more.fields) : []);
	const status = problem.status >= 400 && problem.status <= 599 ? problem.status : 502;
	return fail(status, { action, ok: false, problem, ...(more.id ? { id: more.id } : {}) });
}

/** The secret of a creation answer, if the API returns one that it will not show again. */
function secretOf(
	noun: string,
	output: JsonSchema | undefined,
	view: JsonSchema | undefined,
	data: unknown
): OnceSecret | undefined {
	if (!isRecord(data)) return undefined;
	for (const key of onceOnly(spec, output, view)) {
		const value = data[key];
		if (typeof value === 'string' && value) {
			return {
				title: `${capitalise(noun)} “${defaultName(data)}”`,
				value,
				note: `This is the only time the ${key} of this ${noun} is shown. Copy it now and store it where the calling service reads its secrets.`
			};
		}
	}
	return undefined;
}

export async function overridesFor(
	fields: Overrides | undefined,
	event: KitEvent
): Promise<FieldOverrides> {
	if (!fields) return {};
	return typeof fields === 'function' ? await fields(event) : fields;
}

function formField(form: FormData, name: string): string {
	const value = form.get(name);
	return typeof value === 'string' ? value : '';
}

/**
 * The form fields for the request body of one operation: for a dialog around an action that
 * is not create or replace, such as rotating a key.
 */
export function inputFields(
	path: keyof paths,
	method: 'post' | 'put' = 'post',
	overrides: FieldOverrides = {}
): Field[] {
	const schema = spec.paths[path]?.[method]?.requestBody?.content?.['application/json']?.schema;
	return schema === undefined ? [] : buildFields(spec, schema, { overrides });
}

/** A value of a submitted form as text; empty when it is missing. */
export function formText(form: FormData, name: string): string {
	return formField(form, name);
}

/** `load` and `actions` for a page that manages the objects of one collection. */
export function resource<
	P extends CollectionPath,
	E extends Record<string, unknown> = Record<never, never>
>(options: ResourceOptions<P, E>) {
	const shape = describeResource(spec, options.path);
	const scope = options.writers ?? scopeOfPath(options.path);
	const noun = options.noun;
	const nameOf = async (item: unknown, event: KitEvent) =>
		(options.nameOf ? await options.nameOf(item as ItemOf<P>, event) : defaultName(item)) ||
		'this object';

	// The version is not a control of the form: it travels with it, hidden.
	const fieldsFor = (input: JsonSchema, mode: FormMode, overrides: FieldOverrides) =>
		buildFields(spec, input, { overrides: { version: { hidden: true }, ...overrides }, mode });

	async function load(event: KitEvent): Promise<ResourceData<ItemOf<P>> & E> {
		const principal = event.locals.principal;
		const params = pathValues(shape.params, event);
		const query = queryFromUrl(shape.listQuery, event.url, {
			limit: String(options.pageSize ?? 50)
		});

		const [result, overrides, extra] = await Promise.all([
			loose(event.locals.api).GET(shape.collection, { params: { path: params, query } }),
			overridesFor(options.fields, event),
			options.extra ? options.extra(event) : Promise.resolve({} as E)
		]);

		let items: ItemOf<P>[] = [];
		let nextCursor: string | null = null;
		let problem: ApiProblem | null = null;
		if (result.response.ok && isRecord(result.data)) {
			items = (result.data.items ?? []) as ItemOf<P>[];
			nextCursor = typeof result.data.next_cursor === 'string' ? result.data.next_cursor : null;
		} else {
			problem = problemOf(event, result);
			if (problem.status === 404) error(404, problem.message);
		}

		const writable = principal !== null && problem === null && canWrite(principal, scope, params);
		return {
			...extra,
			noun,
			items,
			cursor: event.url.searchParams.get('cursor'),
			nextCursor,
			problem,
			createFields:
				writable && shape.create ? fieldsFor(shape.create.input, 'create', overrides) : null,
			replaceFields:
				writable && shape.replace ? fieldsFor(shape.replace.input, 'replace', overrides) : null,
			canDelete: writable && shape.remove
		};
	}

	const actions = {
		create: async (event: KitEvent): Promise<Outcome> => {
			if (!shape.create) error(405, `A ${noun} cannot be created through the API.`);
			const params = pathValues(shape.params, event);
			const form = await event.request.formData();
			const fields = fieldsFor(
				shape.create.input,
				'create',
				await overridesFor(options.fields, event)
			);
			const read = readForm(fields, form);
			const result = await loose(event.locals.api).POST(shape.collection, {
				params: { path: params },
				body: options.prepare ? options.prepare(read) : read
			});
			if (!result.response.ok) return failed(event, 'create', result, { fields });
			const secret = secretOf(noun, shape.create.output, shape.view, result.data);
			return done(
				'create',
				`${capitalise(noun)} “${await nameOf(result.data, event)}” was created.${appliedNote(result.response)}`,
				{
					...(secret ? { secret } : {})
				}
			);
		},

		replace: async (event: KitEvent): Promise<Outcome> => {
			if (!shape.replace || !shape.item || !shape.idParam) {
				error(405, `A ${noun} cannot be changed through the API.`);
			}
			const params = pathValues(shape.params, event);
			const form = await event.request.formData();
			const id = formField(form, 'id');
			const fields = fieldsFor(
				shape.replace.input,
				'replace',
				await overridesFor(options.fields, event)
			);
			const read = readForm(fields, form);
			const body: Record<string, unknown> = options.prepare ? options.prepare(read) : read;
			// The version the form was opened with: the core refuses the change if the object
			// has moved on since.
			const version = Number(formField(form, 'version'));
			if (formField(form, 'version') !== '' && Number.isInteger(version)) body.version = version;
			const result = await loose(event.locals.api).PUT(shape.item, {
				params: { path: { ...params, [shape.idParam]: id } },
				body
			});
			if (!result.response.ok) return failed(event, 'replace', result, { id, fields });
			return done(
				'replace',
				`${capitalise(noun)} “${await nameOf(result.data, event)}” was saved.${appliedNote(result.response)}`,
				{
					id
				}
			);
		},

		delete: async (event: KitEvent): Promise<Outcome> => {
			if (!shape.remove || !shape.item || !shape.idParam) {
				error(405, `A ${noun} cannot be deleted through the API.`);
			}
			const params = pathValues(shape.params, event);
			const form = await event.request.formData();
			const id = formField(form, 'id');
			const result = await loose(event.locals.api).DELETE(shape.item, {
				params: { path: { ...params, [shape.idParam]: id } }
			});
			if (!result.response.ok) return failed(event, 'delete', result, { id });
			const name = formField(form, 'name');
			return done(
				'delete',
				`${capitalise(noun)} ${name ? `“${name}” ` : ''}was deleted.${appliedNote(result.response)}`,
				{ id }
			);
		}
	};

	return { load, actions, shape };
}

export interface ObjectOptions<P extends ObjectPath> {
	path: P;
	/** What the object is called in messages: "workspace", "settings". */
	noun: string;
	fields?: Overrides;
	/**
	 * Who may write, when it is not what the path suggests: a workspace is in its
	 * organisation's collection, but its own admins keep its settings.
	 */
	writers?: 'platform' | 'organisation' | 'workspace';
}

export interface ObjectData<T> {
	noun: string;
	item: T | null;
	problem: ApiProblem | null;
	/** The form to change the object; `null` when the caller's role does not allow it. */
	fields: Field[] | null;
	/** The same form, for showing the values to someone who may not change them. */
	readFields: Field[];
}

/** `load` and a `save` action for a page that shows and replaces one object. */
export function objectResource<P extends ObjectPath>(options: ObjectOptions<P>) {
	const shape = describeObject(spec, options.path);
	const scope = options.writers ?? scopeOfPath(options.path);
	const noun = options.noun;

	const fieldsFor = (overrides: FieldOverrides) =>
		shape.replace
			? buildFields(spec, shape.replace.input, {
					overrides: { version: { hidden: true }, ...overrides },
					mode: 'replace'
				})
			: [];

	async function load(event: KitEvent): Promise<ObjectData<ObjectOf<P>>> {
		const principal = event.locals.principal;
		const params = pathValues(shape.params, event);
		const [result, overrides] = await Promise.all([
			loose(event.locals.api).GET(shape.path, { params: { path: params } }),
			overridesFor(options.fields, event)
		]);
		let item: ObjectOf<P> | null = null;
		let problem: ApiProblem | null = null;
		if (result.response.ok) {
			item = result.data as ObjectOf<P>;
		} else {
			problem = problemOf(event, result);
			if (problem.status === 404) error(404, problem.message);
		}
		const fields = fieldsFor(overrides);
		const writable =
			principal !== null &&
			problem === null &&
			shape.replace !== null &&
			canWrite(principal, scope, params);
		return { noun, item, problem, fields: writable ? fields : null, readFields: fields };
	}

	const actions = {
		save: async (event: KitEvent): Promise<Outcome> => {
			if (!shape.replace) error(405, `The ${noun} cannot be changed through the API.`);
			const params = pathValues(shape.params, event);
			const form = await event.request.formData();
			const fields = fieldsFor(await overridesFor(options.fields, event));
			const body: Record<string, unknown> = readForm(fields, form);
			const version = Number(formField(form, 'version'));
			if (formField(form, 'version') !== '' && Number.isInteger(version)) body.version = version;
			const result = await loose(event.locals.api).PUT(shape.path, {
				params: { path: params },
				body
			});
			if (!result.response.ok) return failed(event, 'save', result, { fields });
			return done('save', `Your changes to the ${noun} were saved.${appliedNote(result.response)}`);
		}
	};

	return { load, actions, shape };
}
