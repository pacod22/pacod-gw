// Languages: the page opens in the language the service chose (the person's cookie, their
// browser's, or the realm's), written into <html lang>. Choosing another sets the cookie and
// loads the page again.

import { locales, setLocale, type Locale } from '#lib/paraglide/runtime.js';

/** Each language in its own name. */
export const LANGUAGE_NAMES: Record<string, string> = {
	en: 'English',
	bg: 'Български',
	de: 'Deutsch',
	ru: 'Русский'
};

export const COOKIE = 'pid_lang';

export function isLocale(code: string | null | undefined): code is Locale {
	return !!code && (locales as readonly string[]).includes(code);
}

/** The language of the page, as the service wrote it. */
export function pageLocale(): Locale {
	const lang = document.documentElement.lang;
	return isLocale(lang) ? lang : 'en';
}

export function chooseLanguage(code: string): void {
	if (!isLocale(code)) return;
	document.cookie = `${COOKIE}=${code}; Path=/; Max-Age=31536000; SameSite=Lax`;
	setLocale(code);
}
