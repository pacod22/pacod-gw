// What to tell a person about an answer of the service, by its error code.

import { ApiError } from '#lib/api.ts';
import { m } from '#lib/paraglide/messages.js';

export function explain(e: unknown): string {
	if (!(e instanceof ApiError)) return m.error_network();
	const min = Number(e.details.min_length ?? 0);
	switch (e.code) {
		case 'invalid_credentials':
			return m.error_invalid_credentials();
		case 'too_many_attempts':
			return m.error_too_many();
		case 'account_disabled':
			return m.error_account_disabled();
		case 'password_too_short':
			return m.error_password_too_short({ min });
		case 'password_too_long':
			return m.error_password_too_long();
		case 'password_too_common':
			return m.error_password_too_common();
		case 'bad_email':
			return m.error_bad_email();
		case 'flow_expired':
			return m.error_flow_expired();
		case 'link_invalid':
			return m.error_link_invalid();
		case 'signup_closed':
			return m.error_signup_closed();
		case 'terms_not_accepted':
			return m.error_terms();
		case 'email_taken':
			return m.error_email_taken();
		case 'same_email':
			return m.error_same_email();
		case 'reauth_required':
			return m.error_reauth();
		case 'last_holder':
			return m.error_last_holder();
		case 'last_sign_in_method':
			return m.error_last_method();
		case 'sign_in_required':
			return m.error_sign_in_required();
		case 'wrong_account':
			return m.error_wrong_account();
		case 'not_allowed':
			return m.error_not_allowed();
		case 'google_unavailable':
			return m.error_google_unavailable();
		case 'email_unavailable':
			return m.error_email_unavailable();
		default:
			return m.error_generic();
	}
}

/** The messages for a `?error=` the service sends the sign-in page after Google. */
export function googleError(code: string | null): string | null {
	switch (code) {
		case null:
			return null;
		case 'google_cancelled':
			return m.google_cancelled();
		case 'google_unverified':
			return m.google_unverified();
		case 'google_no_account':
			return m.google_no_account();
		case 'account_disabled':
			return m.error_account_disabled();
		default:
			return m.error_generic();
	}
}
