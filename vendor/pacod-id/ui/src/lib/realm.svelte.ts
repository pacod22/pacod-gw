// The realm's description (GET /ui-api/v1/realm), read once per page load.

import { get } from '#lib/api.ts';

export interface Realm {
	name: string;
	issuer: string;
	brand: 'gateway' | 'cms' | 'kb' | 'shop' | 'bazaar';
	locales: string[];
	default_locale: string;
	self_signup: boolean;
	google: boolean;
	terms_url: string | null;
	privacy_url: string | null;
	password_min_length: number;
	role_holder_password_min_length: number;
}

export const realm = $state<{ value: Realm | null }>({ value: null });

let loading: Promise<Realm> | null = null;

export function loadRealm(): Promise<Realm> {
	loading ??= get<Realm>('realm').then((r) => (realm.value = r));
	return loading;
}
