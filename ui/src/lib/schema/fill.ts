/**
 * Writes values into the controls of a schema-driven form, by field path: what "start from a
 * preset" does. The controls are the ones `SchemaField` renders, so a path names them.
 */

function flatten(value: unknown, prefix: string, out: Map<string, unknown>): void {
	if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
		for (const [key, inner] of Object.entries(value)) {
			flatten(inner, prefix ? `${prefix}.${key}` : key, out);
		}
	} else {
		out.set(prefix, value);
	}
}

/** `{ quirks: { stream_usage: true } }` as `quirks.stream_usage → true`. */
export function flattenValues(values: Record<string, unknown>, prefix = ''): Map<string, unknown> {
	const out = new Map<string, unknown>();
	flatten(values, prefix, out);
	return out;
}

/** Sets the form's controls and tells them, so that anything that follows a choice follows. */
export function fillForm(
	form: HTMLFormElement,
	values: Record<string, unknown>,
	prefix = ''
): void {
	for (const [path, value] of flattenValues(values, prefix)) {
		const controls = form.querySelectorAll<
			HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement
		>(`[name="${CSS.escape(path)}"]`);
		for (const control of controls) {
			if (control instanceof HTMLInputElement && control.type === 'checkbox') {
				// One box is a boolean; several with one name are the members of a list.
				control.checked = Array.isArray(value)
					? value.map(String).includes(control.value)
					: value === true;
			} else if (Array.isArray(value)) {
				control.value = value.join(control instanceof HTMLTextAreaElement ? '\n' : ', ');
			} else {
				control.value = value === null || value === undefined ? '' : String(value);
			}
			control.dispatchEvent(new Event('input', { bubbles: true }));
			control.dispatchEvent(new Event('change', { bubbles: true }));
		}
	}
}
