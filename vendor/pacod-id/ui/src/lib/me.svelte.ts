// The signed-in person (GET /ui-api/v1/account), for the account and admin pages.

import { ApiError, get, post } from '#lib/api.ts';
import { go } from '#lib/page.ts';

export interface Me {
	account: {
		id: string;
		email: string;
		display_name: string;
		locale: string;
		email_verified: boolean;
		has_password: boolean;
		created_at: string;
	};
	session: { id: string; auth_time: string; recent: boolean };
	admin: { accounts: boolean; service_accounts: boolean; audit: boolean };
}

export const me = $state<{ value: Me | null }>({ value: null });

/** Reads the person, or sends the browser to sign in and come back here. */
export async function loadMe(): Promise<Me | null> {
	try {
		me.value = await get<Me>('account');
		return me.value;
	} catch (e) {
		if (e instanceof ApiError && e.status === 401) {
			const back = window.location.pathname + window.location.search;
			go((await post<{ to: string }>('account/sign-in', { return_to: back })).to);
			return null;
		}
		throw e;
	}
}

export async function signOut(): Promise<void> {
	await post('logout', {});
	me.value = null;
	window.location.assign('/account');
}
