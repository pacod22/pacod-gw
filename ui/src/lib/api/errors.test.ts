import { describe, expect, it } from 'vitest';
import { fieldErrors, localProblem, sentence, toProblem, UNREACHABLE } from './errors';

const body = (type: string, message: string, details?: unknown) => ({
	error: details === undefined ? { type, message } : { type, message, details }
});

describe('sentence', () => {
	it('makes a sentence of the core’s fragments', () => {
		expect(sentence('your role does not allow this')).toBe('Your role does not allow this.');
		expect(sentence('Already a sentence.')).toBe('Already a sentence.');
		expect(sentence('session_hours must be between 1 and 720')).toBe(
			'session_hours must be between 1 and 720.'
		);
		expect(sentence('  ')).toBe('');
	});
});

describe('toProblem', () => {
	it('puts validation failures next to the form, at the field they name', () => {
		const problem = toProblem(
			422,
			body('validation_failed', 'name must be lower-case letters, digits and hyphens'),
			['name', 'display_name']
		);
		expect(problem).toMatchObject({
			status: 422,
			type: 'validation_failed',
			placement: 'form',
			tone: 'error',
			reload: false,
			message: 'Name must be lower-case letters, digits and hyphens.',
			fields: { name: 'Name must be lower-case letters, digits and hyphens.' }
		});
	});

	it('keeps a validation message that names no field for the form as a whole', () => {
		const problem = toProblem(
			422,
			body('validation_failed', 'this role cannot be given in this kind of scope'),
			['principal', 'role', 'scope']
		);
		expect(problem.placement).toBe('form');
		expect(problem.fields).toEqual({});
	});

	it('reads a malformed request as a problem of the form', () => {
		const problem = toProblem(400, body('invalid_request', 'missing field `name`'), ['name']);
		expect(problem.placement).toBe('form');
		expect(problem.fields).toEqual({ name: 'Missing field `name`.' });
	});

	it('offers a reload on conflicts and version mismatches', () => {
		const conflict = toProblem(409, body('conflict', 'the organisation still has workspaces'), [
			'name'
		]);
		expect(conflict).toMatchObject({
			type: 'conflict',
			placement: 'form',
			tone: 'warning',
			reload: true
		});
		expect(conflict.hint).toMatch(/Reload/);
		// A conflict is about the object, not about one field of the form.
		expect(conflict.fields).toEqual({});

		const stale = toProblem(
			409,
			body('version_mismatch', 'the object was changed by someone else')
		);
		expect(stale).toMatchObject({ type: 'version_mismatch', reload: true, placement: 'form' });
		expect(stale.title).toMatch(/Someone else/);
		expect(stale.hint).toMatch(/current version/);
	});

	it('shows forbidden and unavailable as notices', () => {
		const forbidden = toProblem(403, body('forbidden', 'your role does not allow this'));
		expect(forbidden).toMatchObject({ placement: 'notice', tone: 'warning', reload: false });

		const unavailable = toProblem(
			503,
			body('unavailable', 'the analytics service is not configured in this installation')
		);
		expect(unavailable).toMatchObject({
			type: 'unavailable',
			placement: 'notice',
			tone: 'info',
			message: 'The analytics service is not configured in this installation.'
		});
		expect(unavailable.hint).toMatch(/not affected/);
	});

	it('explains a locked account and a wrong password with the core’s words', () => {
		expect(toProblem(401, body('unauthorized', 'wrong email or password')).message).toBe(
			'Wrong email or password.'
		);
		const locked = toProblem(
			429,
			body('too_many_attempts', 'too many failed logins for this account; try again later')
		);
		expect(locked.type).toBe('too_many_attempts');
		expect(locked.message).toMatch(/too many failed logins/i);
		expect(locked.hint).toMatch(/Wait/);
	});

	it('reads the analytics service’s errors, which use `code`', () => {
		const down = toProblem(503, {
			error: { code: 'storage_unavailable', message: 'ClickHouse did not accept the request' }
		});
		// An unknown type is judged by its status.
		expect(down).toMatchObject({ type: 'storage_unavailable', placement: 'notice', tone: 'info' });

		const filter = toProblem(
			400,
			{ error: { code: 'invalid_parameter', message: 'from: expected an RFC 3339 time' } },
			['from', 'to']
		);
		expect(filter.placement).toBe('form');
		expect(filter.fields).toEqual({ from: 'From: expected an RFC 3339 time.' });
	});

	it('falls back on the status when the body is not the core’s', () => {
		expect(toProblem(403, '<html>proxy</html>')).toMatchObject({
			type: 'forbidden',
			placement: 'notice'
		});
		expect(toProblem(404, undefined)).toMatchObject({ type: 'not_found', reload: true });
		const broken = toProblem(500, null);
		expect(broken).toMatchObject({ type: 'internal', tone: 'error' });
		expect(broken.message).toBe('The gateway answered with status 500.');
		expect(toProblem(418, {})).toMatchObject({ type: 'error', placement: 'notice' });
	});

	it('says when the core did not answer at all', () => {
		const problem = toProblem(
			502,
			body(UNREACHABLE, 'the management API of the gateway did not answer')
		);
		expect(problem).toMatchObject({ type: UNREACHABLE, placement: 'notice', tone: 'error' });
		expect(problem.hint).toMatch(/GATEWAY_API_URL/);
	});

	it('never puts field messages on a notice', () => {
		const problem = toProblem(403, body('forbidden', 'name is not yours to change'), ['name']);
		expect(problem.fields).toEqual({});
	});
});

describe('details of an error', () => {
	it('lists what a conflict names, such as the keys that still use a template', () => {
		const problem = toProblem(
			409,
			body('conflict', 'the template is named by API keys', { keys: ['chat', 'batch'] })
		);
		expect(problem.lists).toEqual([{ label: 'keys', items: ['chat', 'batch'] }]);
		expect(toProblem(409, body('conflict', 'x', { keys: [] })).lists).toEqual([]);
		expect(toProblem(409, body('conflict', 'x')).lists).toEqual([]);
	});

	it('puts a wrong current password at its field, as the core now names it', () => {
		const problem = toProblem(
			422,
			body('validation_failed', 'the current password is wrong', { field: 'current' }),
			['current', 'new']
		);
		expect(problem.fields).toEqual({ current: 'The current password is wrong.' });
	});
});

describe('fieldErrors', () => {
	const names = ['name', 'settings.unsupported_params', 'settings.locked', 'tag_keys'];

	it('takes fields from details when the API names them', () => {
		expect(
			fieldErrors('invalid', { fields: { name: 'too long', tag_keys: 'at most eight' } }, names)
		).toEqual({
			name: 'Too long.',
			tag_keys: 'At most eight.'
		});
		expect(fieldErrors('must be a slug', { field: 'name' }, names)).toEqual({
			name: 'Must be a slug.'
		});
		expect(
			fieldErrors('invalid', [{ path: 'settings.locked', message: 'unknown setting' }], names)
		).toEqual({ 'settings.locked': 'Unknown setting.' });
	});

	it('finds a nested field by its last segment', () => {
		expect(fieldErrors('locked names an unknown setting', undefined, names)).toEqual({
			'settings.locked': 'Locked names an unknown setting.'
		});
	});

	it('does not guess when the message names no field or several', () => {
		expect(fieldErrors('the workspace still has keys', undefined, names)).toEqual({});
		expect(fieldErrors('use either `name` or `tag_keys`', undefined, names)).toEqual({});
		expect(fieldErrors('', undefined, names)).toEqual({});
	});
});

describe('localProblem', () => {
	it('is a form problem found before anything was sent', () => {
		expect(localProblem('Enter a name.', { name: 'Required.' })).toMatchObject({
			status: 400,
			placement: 'form',
			message: 'Enter a name.',
			fields: { name: 'Required.' }
		});
	});
});

describe('account emails', () => {
	it('says that an email failed, with what to do instead', () => {
		const problem = toProblem(
			502,
			body('email_failed', 'the mail server refused the message: connection refused')
		);
		expect(problem).toMatchObject({
			type: 'email_failed',
			title: 'The email was not sent',
			placement: 'notice',
			tone: 'error'
		});
		expect(problem.hint).toContain('email-check');
	});

	it('tells apart an installation without email from an unavailable service', () => {
		const problem = toProblem(
			503,
			body('email_unavailable', 'this installation does not send email')
		);
		expect(problem.title).toBe('This installation does not send email');
		expect(problem.tone).toBe('warning');
	});
});
