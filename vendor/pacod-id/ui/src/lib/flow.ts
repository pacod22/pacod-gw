// A sign-in flow as GET /ui-api/v1/flows/{id} describes it, and what its steps answer.

export interface Flow {
	id: string;
	stage: 'start' | 'google' | 'link_google' | 'verify_email' | 'change_password' | string;
	realm: string;
	brand: string;
	locales: string[];
	login_hint: string | null;
	client: string;
	self_signup: boolean;
	password_min_length: number;
	terms_url: string | null;
	privacy_url: string | null;
	google: boolean;
	link_email: string | null;
}

export type Next =
	| { next: 'redirect'; to: string }
	| { next: 'verify_email'; email: string }
	| { next: 'change_password'; min_length: number };
