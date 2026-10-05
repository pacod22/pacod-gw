/**
 * Reads what a resource offers from the OpenAPI document: which operations exist, which
 * schemas they take and return, and which query parameters a listing accepts.
 */
import { resolveSchema, type JsonSchema, type SchemaSource } from './form';

interface MediaTypes {
	content?: Record<string, { schema?: JsonSchema }>;
}

export interface Operation {
	operationId?: string;
	summary?: string;
	parameters?: { name: string; in: string; required?: boolean; description?: string }[];
	requestBody?: MediaTypes;
	responses?: Record<string, MediaTypes>;
}

export interface OpenApiDoc extends SchemaSource {
	servers?: { url: string }[];
	paths: Record<string, Record<string, Operation>>;
}

export interface ResourceShape {
	/** The collection path, such as `/orgs/{org}/workspaces`. */
	collection: string;
	/** The path of one object, such as `/orgs/{org}/workspaces/{ws}`. */
	item: string | null;
	/** The name of the path parameter that carries the object's id. */
	idParam: string | null;
	/** The path parameters of the collection. */
	params: string[];
	/** Query parameters the listing accepts. */
	listQuery: string[];
	/** The object as the API returns it. */
	view: JsonSchema | undefined;
	create: { input: JsonSchema; output: JsonSchema | undefined } | null;
	replace: { input: JsonSchema } | null;
	remove: boolean;
}

function jsonSchema(media: MediaTypes | undefined): JsonSchema | undefined {
	return media?.content?.['application/json']?.schema;
}

function successSchema(operation: Operation | undefined): JsonSchema | undefined {
	const responses = operation?.responses ?? {};
	const status = Object.keys(responses).find((code) => /^2\d\d$/.test(code));
	return status ? jsonSchema(responses[status]) : undefined;
}

/** The names in braces of a path template. */
export function pathParams(path: string): string[] {
	return [...path.matchAll(/\{([^}]+)\}/g)].map((m) => m[1]);
}

/** The query parameters an operation declares. */
export function queryParams(doc: OpenApiDoc, path: string, method = 'get'): string[] {
	return (doc.paths[path]?.[method]?.parameters ?? [])
		.filter((p) => p.in === 'query')
		.map((p) => p.name);
}

/** Describes the collection at `path` and the single objects below it. */
export function describeResource(doc: OpenApiDoc, path: string): ResourceShape {
	const collection = doc.paths[path];
	if (!collection) throw new Error(`the OpenAPI document has no path ${path}`);

	// The item path is the collection path plus exactly one parameter.
	const prefix = `${path}/{`;
	const item =
		Object.keys(doc.paths).find(
			(p) => p.startsWith(prefix) && p.endsWith('}') && !p.slice(prefix.length).includes('/')
		) ?? null;
	const itemOps = item ? doc.paths[item] : undefined;

	const listSchema = resolveSchema(doc, successSchema(collection.get)).schema;
	const listItems = listSchema.properties?.items;
	const itemsOfList = typeof listItems === 'object' ? listItems.items : undefined;
	const view = successSchema(itemOps?.get) ?? itemsOfList;

	const createInput = jsonSchema(collection.post?.requestBody);
	const replaceInput = jsonSchema(itemOps?.put?.requestBody);

	return {
		collection: path,
		item,
		idParam: item ? (pathParams(item).pop() ?? null) : null,
		params: pathParams(path),
		listQuery: queryParams(doc, path),
		view,
		create: createInput ? { input: createInput, output: successSchema(collection.post) } : null,
		replace: replaceInput ? { input: replaceInput } : null,
		remove: Boolean(itemOps?.delete)
	};
}

export interface ObjectShape {
	path: string;
	params: string[];
	view: JsonSchema | undefined;
	replace: { input: JsonSchema } | null;
}

/** Describes a single object that is read with GET and written with PUT at the same path. */
export function describeObject(doc: OpenApiDoc, path: string): ObjectShape {
	const operations = doc.paths[path];
	if (!operations) throw new Error(`the OpenAPI document has no path ${path}`);
	const input = jsonSchema(operations.put?.requestBody);
	return {
		path,
		params: pathParams(path),
		view: successSchema(operations.get),
		replace: input ? { input } : null
	};
}

/**
 * The properties an answer carries exactly once: those of `output` that the regular view of
 * the object does not have, such as the `secret` of a new API key.
 */
export function onceOnly(
	doc: SchemaSource,
	output: JsonSchema | undefined,
	view: JsonSchema | undefined
): string[] {
	if (output === undefined || view === undefined) return [];
	const shown = resolveSchema(doc, view).schema.properties ?? {};
	const returned = resolveSchema(doc, output).schema.properties ?? {};
	return Object.keys(returned).filter((key) => !(key in shown));
}

/**
 * Who may write to a path, judged by where it lies: below a workspace, below an
 * organisation, or at the top.
 */
export function scopeOfPath(path: string): 'platform' | 'organisation' | 'workspace' {
	// A single object is governed like the collection it is in.
	const collection = path.replace(/\/\{[^}]+\}$/, '');
	if (/^\/orgs\/\{[^}]+\}\/workspaces\/\{[^}]+\}\//.test(collection)) return 'workspace';
	if (/^\/orgs\/\{[^}]+\}\//.test(collection)) return 'organisation';
	return 'platform';
}
