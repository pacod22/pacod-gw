/**
 * Schema to form.
 *
 * `buildFields` turns a JSON Schema of the OpenAPI document into a description of form
 * controls, and `readForm` turns the submitted form back into the JSON body the API expects.
 * Both are pure, so the same description drives the rendering in the browser and the parsing
 * on the server.
 */

export type JsonSchema = boolean | SchemaObject;

export interface SchemaObject {
	$ref?: string;
	type?: string | string[];
	title?: string;
	description?: string;
	default?: unknown;
	const?: unknown;
	enum?: unknown[];
	format?: string;
	minimum?: number;
	maximum?: number;
	writeOnly?: boolean;
	readOnly?: boolean;
	properties?: Record<string, JsonSchema>;
	required?: string[];
	items?: JsonSchema;
	oneOf?: JsonSchema[];
	anyOf?: JsonSchema[];
	allOf?: JsonSchema[];
	additionalProperties?: JsonSchema;
}

/** The part of an OpenAPI document that `$ref`s point into. */
export interface SchemaSource {
	components?: { schemas?: Record<string, JsonSchema> };
}

export type FieldKind =
	| 'text' // a string
	| 'secret' // a string the API never returns: sent only when filled
	| 'number'
	| 'boolean'
	| 'enum' // one of a fixed set of strings
	| 'list' // an array of strings or numbers, one per line
	| 'map' // an object of free keys with string values, one `key: value` per line
	| 'choices' // an array of enum values, as checkboxes
	| 'group' // a nested object
	| 'union' // one of several objects, told apart by a discriminator property
	| 'json'; // anything else, as JSON text

export interface Option {
	value: string;
	label: string;
	help: string;
}

export interface Variant extends Option {
	fields: Field[];
}

export interface Field {
	kind: FieldKind;
	/** Property name in the parent object. */
	key: string;
	/** Dot path from the root of the body. It is the name of the form control. */
	path: string;
	label: string;
	help: string;
	required: boolean;
	nullable: boolean;
	default?: unknown;
	/** `text`: the schema's format, such as `date-time`. */
	format?: string;
	/** `number`: whole numbers only. `list`: the items are numbers. */
	integer?: boolean;
	/** `list`: the items are numbers. */
	numeric?: boolean;
	min?: number;
	max?: number;
	/** `enum` and `choices` */
	options?: Option[];
	/** `group` */
	fields?: Field[];
	/** `union`: the name of the property that tells the variants apart. */
	discriminator?: string;
	variants?: Variant[];
}

/** Changes to a field that the schema cannot express. Keys are field paths. */
export interface FieldOverride {
	label?: string;
	help?: string;
	/** Leave the field out of the form; the API's default applies. */
	hidden?: boolean;
	/** `'create'`: required when creating, optional when replacing. */
	required?: boolean | 'create';
	/** Offer these values instead of free text, for example the ids of existing objects. */
	options?: Option[];
}

/**
 * Keyed by field path. A field inside a union variant can be addressed for one variant only
 * as `path@variant`, for example `scope.id@organisation`.
 */
export type FieldOverrides = Record<string, FieldOverride>;

export type FormMode = 'create' | 'replace';

export interface BuildOptions {
	overrides?: FieldOverrides;
	mode?: FormMode;
}

const ACRONYMS = new Set(['api', 'id', 'ip', 'kek', 'uri', 'url', 'utc']);

/** `base_url` becomes "Base URL". */
export function humanise(name: string): string {
	const words = name.split(/[_\-.\s]+/).filter(Boolean);
	return words
		.map((word, index) => {
			const lower = word.toLowerCase();
			if (ACRONYMS.has(lower)) return lower.toUpperCase();
			return index === 0 ? lower[0].toUpperCase() + lower.slice(1) : lower;
		})
		.join(' ');
}

function isObject(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isNull(schema: JsonSchema): boolean {
	return typeof schema === 'object' && schema.type === 'null';
}

interface Resolved {
	schema: SchemaObject;
	nullable: boolean;
}

/**
 * Follows `$ref`, unwraps "X or null" and single-member `allOf`. Keywords next to a `$ref`
 * (OpenAPI 3.1 allows a description or a default there) win over those of the target.
 */
export function resolveSchema(source: SchemaSource, input: JsonSchema | undefined): Resolved {
	let schema: SchemaObject = typeof input === 'object' && input !== null ? input : {};
	let nullable = false;
	for (let depth = 0; depth < 32; depth++) {
		if (schema.$ref) {
			const { $ref, ...siblings } = schema;
			const name = $ref.replace(/^#\/components\/schemas\//, '');
			const target = source.components?.schemas?.[name];
			schema = { ...(typeof target === 'object' && target !== null ? target : {}), ...siblings };
			continue;
		}
		if (schema.allOf?.length) {
			// `allOf` of objects is one object with all their properties: the core describes
			// the body of a replace as the input plus the version.
			const { allOf, ...siblings } = schema;
			const merged: SchemaObject = {};
			for (const member of allOf) {
				const part = resolveSchema(source, member).schema;
				Object.assign(merged, part, {
					properties: { ...merged.properties, ...part.properties },
					required: [...(merged.required ?? []), ...(part.required ?? [])]
				});
			}
			if (Object.keys(merged.properties ?? {}).length === 0) delete merged.properties;
			if (merged.required?.length === 0) delete merged.required;
			schema = { ...merged, ...siblings };
			continue;
		}
		const alternatives = schema.anyOf ?? schema.oneOf;
		if (alternatives?.some(isNull)) {
			nullable = true;
			const rest = alternatives.filter((s) => !isNull(s));
			const siblings: SchemaObject = { ...schema };
			delete siblings.anyOf;
			delete siblings.oneOf;
			if (rest.length === 1) {
				const only = rest[0];
				schema = { ...(typeof only === 'object' ? only : {}), ...siblings };
			} else {
				schema = { ...siblings, oneOf: rest };
			}
			continue;
		}
		if (Array.isArray(schema.type)) {
			const types = schema.type.filter((t) => t !== 'null');
			if (types.length !== schema.type.length) nullable = true;
			schema = { ...schema, type: types.length === 1 ? types[0] : types };
			continue;
		}
		break;
	}
	return { schema, nullable };
}

/** The options of a string enum: `enum`, or a `oneOf` of constants that carry descriptions. */
function enumOptions(source: SchemaSource, schema: SchemaObject): Option[] | null {
	if (Array.isArray(schema.enum) && schema.enum.every((v) => typeof v === 'string')) {
		return (schema.enum as string[]).map((value) => ({ value, label: value, help: '' }));
	}
	if (!schema.oneOf?.length) return null;
	const options: Option[] = [];
	for (const member of schema.oneOf) {
		const { schema: m } = resolveSchema(source, member);
		if (typeof m.const === 'string') {
			options.push({ value: m.const, label: m.const, help: m.description ?? '' });
		} else if (Array.isArray(m.enum) && m.enum.every((v) => typeof v === 'string')) {
			for (const value of m.enum as string[]) {
				options.push({ value, label: value, help: m.description ?? '' });
			}
		} else {
			return null;
		}
	}
	return options;
}

/** The property that tells the members of a `oneOf` of objects apart, `type` if possible. */
function discriminatorOf(members: SchemaObject[]): string | null {
	const constant = (m: SchemaObject, key: string): boolean => {
		const p = m.properties?.[key];
		return typeof p === 'object' && typeof p.const === 'string';
	};
	const candidates = ['type', ...Object.keys(members[0]?.properties ?? {})];
	return candidates.find((key) => members.every((m) => constant(m, key))) ?? null;
}

function buildField(
	source: SchemaSource,
	key: string,
	path: string,
	input: JsonSchema,
	required: boolean
): Field {
	const { schema, nullable } = resolveSchema(source, input);
	const base: Field = {
		kind: 'json',
		key,
		path,
		label: schema.title ?? humanise(key),
		help: schema.description ?? '',
		required,
		nullable
	};
	if (schema.default !== undefined) base.default = schema.default;

	const options = enumOptions(source, schema);
	if (options) return { ...base, kind: 'enum', options };

	if (schema.oneOf?.length) {
		const members = schema.oneOf.map((m) => resolveSchema(source, m).schema);
		const discriminator = discriminatorOf(members);
		if (discriminator) {
			const variants = members.map((member): Variant => {
				const tag = member.properties?.[discriminator];
				const value = typeof tag === 'object' ? String(tag.const) : '';
				const rest: SchemaObject = {
					...member,
					properties: Object.fromEntries(
						Object.entries(member.properties ?? {}).filter(([name]) => name !== discriminator)
					)
				};
				return {
					value,
					label: member.title ?? value,
					help: member.description ?? '',
					fields: buildProperties(source, rest, path)
				};
			});
			return { ...base, kind: 'union', discriminator, variants };
		}
		return base;
	}

	const type = Array.isArray(schema.type) ? schema.type[0] : schema.type;
	switch (type) {
		case 'string': {
			// A secret is what the document marks as one. It is sent only when filled.
			if (schema.writeOnly || schema.format === 'password') return { ...base, kind: 'secret' };
			return schema.format
				? { ...base, kind: 'text', format: schema.format }
				: { ...base, kind: 'text' };
		}
		case 'integer':
		case 'number': {
			const field: Field = { ...base, kind: 'number', integer: type === 'integer' };
			if (typeof schema.minimum === 'number') field.min = schema.minimum;
			if (typeof schema.maximum === 'number') field.max = schema.maximum;
			return field;
		}
		case 'boolean':
			return { ...base, kind: 'boolean' };
		case 'array': {
			const { schema: items } = resolveSchema(source, schema.items);
			const choices = enumOptions(source, items);
			if (choices) return { ...base, kind: 'choices', options: choices };
			if (items.type === 'string') return { ...base, kind: 'list' };
			if (items.type === 'integer' || items.type === 'number') {
				return { ...base, kind: 'list', numeric: true, integer: items.type === 'integer' };
			}
			return base;
		}
		case 'object': {
			if (!schema.properties) {
				const values = resolveSchema(source, schema.additionalProperties).schema;
				return values.type === 'string' ? { ...base, kind: 'map' } : base;
			}
			return { ...base, kind: 'group', fields: buildProperties(source, schema, path) };
		}
		default:
			return base;
	}
}

function buildProperties(source: SchemaSource, schema: SchemaObject, prefix: string): Field[] {
	const required = new Set(schema.required ?? []);
	return Object.entries(schema.properties ?? {})
		.filter(([, property]) => !(typeof property === 'object' && property.readOnly))
		.map(([key, property]) =>
			buildField(source, key, prefix ? `${prefix}.${key}` : key, property, required.has(key))
		);
}

function applyOverrides(
	fields: Field[],
	overrides: FieldOverrides,
	mode: FormMode,
	variant?: string
): Field[] {
	const out: Field[] = [];
	for (const field of fields) {
		const override =
			(variant ? overrides[`${field.path}@${variant}`] : undefined) ?? overrides[field.path];
		if (override?.hidden) continue;
		let next: Field = { ...field };
		if (override) {
			if (override.label !== undefined) next.label = override.label;
			if (override.help !== undefined) next.help = override.help;
			if (override.required !== undefined) {
				next.required = override.required === 'create' ? mode === 'create' : override.required;
			}
			if (override.options && (next.kind === 'text' || next.kind === 'enum')) {
				next = { ...next, kind: 'enum', options: override.options };
			}
			// A list of names of existing objects: choose them instead of typing them.
			if (override.options && next.kind === 'list' && !next.numeric) {
				next = { ...next, kind: 'choices', options: override.options };
			}
		}
		if (next.fields) next.fields = applyOverrides(next.fields, overrides, mode, variant);
		if (next.variants) {
			next.variants = next.variants.map((v) => ({
				...v,
				fields: applyOverrides(v.fields, overrides, mode, v.value)
			}));
		}
		out.push(next);
	}
	return out;
}

/** The fields of a form for an object schema. */
export function buildFields(
	source: SchemaSource,
	schema: JsonSchema,
	options: BuildOptions = {}
): Field[] {
	const { schema: root } = resolveSchema(source, schema);
	const fields = buildProperties(source, root, '');
	return applyOverrides(fields, options.overrides ?? {}, options.mode ?? 'create');
}

/** The field with a path, wherever it is nested. */
export function fieldAt(fields: Field[], path: string): Field | undefined {
	for (const field of fields) {
		if (field.path === path) return field;
		const inner = fieldAt(field.fields ?? [], path);
		if (inner) return inner;
	}
	return undefined;
}

/** Every field path of a form, groups and variants included. Used to place error messages. */
export function fieldPaths(fields: Field[]): string[] {
	const out: string[] = [];
	const walk = (list: Field[]) => {
		for (const field of list) {
			out.push(field.path);
			if (field.fields) walk(field.fields);
			for (const variant of field.variants ?? []) walk(variant.fields);
		}
	};
	walk(fields);
	return [...new Set(out)];
}

// ---------------------------------------------------------------------------------------------
// Reading a submitted form

type FormSource = Pick<FormData, 'get' | 'getAll' | 'has'> | URLSearchParams;

function text(data: FormSource, name: string): string {
	const value = data.get(name);
	return typeof value === 'string' ? value : '';
}

/** `datetime-local` has no zone; the console shows and reads these values as UTC. */
export function toRfc3339(value: string): string {
	const v = value.trim();
	if (/^\d{4}-\d\d-\d\dT\d\d:\d\d$/.test(v)) return `${v}:00Z`;
	if (/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?$/.test(v)) return `${v}Z`;
	return v;
}

/** An RFC 3339 time as the value of a `datetime-local` input, in UTC. */
export function toDateTimeInput(value: unknown): string {
	if (typeof value !== 'string' || !value) return '';
	const date = new Date(value);
	return Number.isNaN(date.getTime()) ? '' : date.toISOString().slice(0, 16);
}

/** `key: value` lines (or `key=value`) as an object. Lines without a key are left out. */
export function parseMap(value: string): Record<string, string> {
	const out: Record<string, string> = {};
	for (const line of value.split(/\r?\n/)) {
		const at = line.search(/[:=]/);
		if (at < 1) continue;
		const key = line.slice(0, at).trim();
		if (key) out[key] = line.slice(at + 1).trim();
	}
	return out;
}

/** An object of strings as `key: value` lines. */
export function formatMap(value: unknown): string {
	if (!isObject(value)) return '';
	return Object.entries(value)
		.map(([key, v]) => `${key}: ${String(v)}`)
		.join('\n');
}

/** An empty optional control: `null` where the schema allows it, otherwise left out. */
function empty(field: Field): null | undefined {
	return field.nullable ? null : undefined;
}

function readField(field: Field, data: FormSource): unknown {
	switch (field.kind) {
		case 'text': {
			const raw = text(data, field.path).trim();
			if (raw === '') return field.required ? '' : empty(field);
			return field.format === 'date-time' ? toRfc3339(raw) : raw;
		}
		case 'secret': {
			// Not trimmed: a secret is sent as typed, and only when something was typed.
			const raw = text(data, field.path);
			return raw === '' ? undefined : raw;
		}
		case 'number': {
			const raw = text(data, field.path).trim();
			if (raw === '') return empty(field);
			const number = Number(raw);
			// Not a number: pass it on as typed so that the API reports the problem.
			return Number.isFinite(number) ? number : raw;
		}
		case 'boolean': {
			const raw = text(data, field.path);
			return raw === 'on' || raw === 'true' || raw === '1';
		}
		case 'enum': {
			const raw = text(data, field.path);
			return raw === '' ? empty(field) : raw;
		}
		case 'list': {
			if (field.numeric) {
				return text(data, field.path)
					.split(/[\s,;]+/)
					.filter(Boolean)
					.map((item) => (Number.isFinite(Number(item)) ? Number(item) : item));
			}
			return text(data, field.path)
				.split(/\r?\n/)
				.map((line) => line.trim())
				.filter(Boolean);
		}
		case 'map':
			return parseMap(text(data, field.path));
		case 'choices':
			return data.getAll(field.path).filter((v): v is string => typeof v === 'string' && v !== '');
		case 'group':
			return readForm(field.fields ?? [], data);
		case 'union': {
			const discriminator = field.discriminator ?? 'type';
			const tag = text(data, `${field.path}.${discriminator}`);
			const variant = field.variants?.find((v) => v.value === tag);
			if (!variant) return empty(field);
			return { [discriminator]: tag, ...readForm(variant.fields, data) };
		}
		case 'json': {
			const raw = text(data, field.path).trim();
			if (raw === '') return empty(field);
			try {
				return JSON.parse(raw);
			} catch {
				return raw;
			}
		}
	}
}

/** The JSON body for a submitted form. Controls that were left empty are left out. */
export function readForm(fields: Field[], data: FormSource): Record<string, unknown> {
	const out: Record<string, unknown> = {};
	for (const field of fields) {
		const value = readField(field, data);
		if (value !== undefined) out[field.key] = value;
	}
	return out;
}

// ---------------------------------------------------------------------------------------------
// Showing current values

/** The value at a field's path inside an object the API returned. */
export function valueAt(object: unknown, path: string): unknown {
	let current: unknown = object;
	for (const part of path.split('.')) {
		if (!isObject(current)) return undefined;
		current = current[part];
	}
	return current;
}

/** What a control starts with: the object's value, or the schema default for a new object. */
export function initialValue(field: Field, object: unknown): unknown {
	if (field.kind === 'secret') return '';
	const current = object === undefined || object === null ? undefined : valueAt(object, field.path);
	return current === undefined ? field.default : current;
}
