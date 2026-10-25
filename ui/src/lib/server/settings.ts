/**
 * The settings page of an organisation or a workspace: the object's form split into sections
 * that are saved one at a time, and a danger zone that switches the object off or deletes it.
 *
 * The API replaces the object as a whole, so a section is saved with `replacePart`: the other
 * sections keep what they hold, and a change made in the meantime is refused instead of
 * overwritten.
 */
import { error, fail, redirect } from '@sveltejs/kit';
import { canWrite } from '#lib/access.ts';
import { toProblem, type ApiProblem } from '#lib/api/errors.ts';
import { buildFields, pickFields, readPicked, type Field } from '#lib/schema/form.ts';
import { describeObject } from '#lib/schema/spec.ts';
import { loose } from './api';
import {
	done,
	failed,
	formText,
	overridesFor,
	pathValues,
	problemOf,
	replacePart,
	type KitEvent,
	type ObjectOf,
	type ObjectPath,
	type Outcome,
	type Overrides
} from './resource';
import { spec } from './spec';

export interface SectionSpec {
	key: string;
	title: string;
	description?: string;
	/** The field paths the section shows, in this order. */
	paths: string[];
}

export interface Section {
	key: string;
	title: string;
	description: string;
	/** What the caller may change; empty when they may change nothing here. */
	fields: Field[];
	/** What the caller may see but not change. */
	readFields: Field[];
}

export interface SettingsData<T> {
	noun: string;
	item: T | null;
	problem: ApiProblem | null;
	sections: Section[];
	/** Whether the caller may switch the object on and off. */
	canSetStatus: boolean;
	/** Whether the caller may delete the object, once it is empty. */
	canDelete: boolean;
}

export interface SettingsOptions<P extends ObjectPath> {
	path: P;
	/** "organisation", "workspace". */
	noun: string;
	/** Who may change the object. */
	writers: 'platform' | 'organisation' | 'workspace';
	/** Who may switch it off and delete it, when that is someone else. */
	statusWriters?: 'platform' | 'organisation' | 'workspace';
	deleters: 'platform' | 'organisation' | 'workspace';
	sections: SectionSpec[];
	fields?: Overrides;
	/** Fields the caller may see but not change, by path. */
	readOnly?: (event: KitEvent) => string[] | Promise<string[]>;
	/** Where to go once the object is deleted. */
	afterDelete: (event: KitEvent) => string;
}

export function settingsResource<P extends ObjectPath>(options: SettingsOptions<P>) {
	const shape = describeObject(spec, options.path);
	if (!shape.replace) throw new Error(`${options.path} cannot be replaced`);
	const input = shape.replace.input;

	async function fieldsFor(event: KitEvent): Promise<Field[]> {
		const overrides = await overridesFor(options.fields, event);
		return buildFields(spec, input, {
			overrides: { version: { hidden: true }, ...overrides },
			mode: 'replace'
		});
	}

	/** The fields of one section that the caller may change. */
	function editable(
		all: Field[],
		spec: SectionSpec,
		locked: Set<string>,
		writable: boolean
	): Field[] {
		if (!writable) return [];
		return pickFields(all, spec.paths).filter((f) => !locked.has(f.path));
	}

	async function lockedFor(event: KitEvent): Promise<Set<string>> {
		return new Set(options.readOnly ? await options.readOnly(event) : []);
	}

	function allowed(event: KitEvent, who: SettingsOptions<P>['writers']): boolean {
		const principal = event.locals.principal;
		return principal !== null && canWrite(principal, who, event.params);
	}

	async function load(event: KitEvent): Promise<SettingsData<ObjectOf<P>>> {
		const params = pathValues(shape.params, event);
		const [result, all, locked] = await Promise.all([
			loose(event.locals.api).GET(shape.path, { params: { path: params } }),
			fieldsFor(event),
			lockedFor(event)
		]);
		let item: ObjectOf<P> | null = null;
		let problem: ApiProblem | null = null;
		if (result.response.ok) {
			item = result.data as ObjectOf<P>;
		} else {
			problem = problemOf(event, result);
			if (problem.status === 404) error(404, problem.message);
		}
		const writable = problem === null && allowed(event, options.writers);
		const sections = options.sections.map((s): Section => {
			const fields = editable(all, s, locked, writable);
			const mine = new Set(fields.map((f) => f.path));
			return {
				key: s.key,
				title: s.title,
				description: s.description ?? '',
				fields,
				readFields: pickFields(all, s.paths).filter((f) => !mine.has(f.path))
			};
		});
		return {
			noun: options.noun,
			item,
			problem,
			sections,
			canSetStatus: problem === null && allowed(event, options.statusWriters ?? options.writers),
			canDelete: problem === null && allowed(event, options.deleters)
		};
	}

	/** Puts `values` into the object; the failure, or `null` when it worked. */
	async function change(
		event: KitEvent,
		action: string,
		section: string,
		values: Record<string, unknown>,
		form: FormData,
		fields: Field[] = []
	): Promise<Outcome | null> {
		const outcome = await replacePart(event, shape, action, values, form, { id: section, fields });
		return typeof outcome === 'string' ? null : outcome;
	}

	const actions = {
		save: async (event: KitEvent): Promise<Outcome> => {
			const form = await event.request.formData();
			const key = formText(form, 'section');
			const spec = options.sections.find((s) => s.key === key);
			if (!spec) error(400, 'Unknown section.');
			const fields = editable(await fieldsFor(event), spec, await lockedFor(event), true);
			const failure = await change(event, 'save', key, readPicked(fields, form), form, fields);
			return failure ?? done('save', `${spec.title}: your changes were saved.`, { id: key });
		},

		status: async (event: KitEvent): Promise<Outcome> => {
			const form = await event.request.formData();
			const status = formText(form, 'status') === 'disabled' ? 'disabled' : 'active';
			const failure = await change(event, 'status', 'status', { status }, form);
			const noun = options.noun[0].toUpperCase() + options.noun.slice(1);
			return (
				failure ??
				done('status', status === 'active' ? `${noun} enabled.` : `${noun} disabled.`, {
					id: 'status'
				})
			);
		},

		delete: async (event: KitEvent): Promise<Outcome> => {
			const params = pathValues(shape.params, event);
			const form = await event.request.formData();
			const api = loose(event.locals.api);
			const current = await api.GET(shape.path, { params: { path: params } });
			if (!current.response.ok) return failed(event, 'delete', current, { id: 'delete' });
			const name = (current.data as { name?: string }).name ?? '';
			if (formText(form, 'confirm').trim() !== name) {
				const problem = toProblem(422, {
					error: { type: 'validation_failed', message: `type the name “${name}” to confirm` }
				});
				return fail(422, { action: 'delete', ok: false, id: 'delete', problem });
			}
			const result = await api.DELETE(shape.path, { params: { path: params } });
			if (!result.response.ok) return failed(event, 'delete', result, { id: 'delete' });
			redirect(303, options.afterDelete(event));
		}
	};

	return { load, actions };
}

/** How many objects a collection holds, up to `cap`; `null` when it cannot be read. */
export async function countOf(
	event: KitEvent,
	path: string,
	params: Record<string, string>,
	cap = 500
): Promise<{ count: number; more: boolean } | null> {
	const result = await loose(event.locals.api).GET(path, {
		params: { path: params, query: { limit: String(cap) } }
	});
	if (!result.response.ok) return null;
	const data = result.data as { items?: unknown[]; next_cursor?: string | null };
	return { count: data.items?.length ?? 0, more: Boolean(data.next_cursor) };
}
