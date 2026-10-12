// Paraglide reads the language from <html lang>, which the service filled in, and remembers a
// choice in the cookie the service reads. Registered before the app starts.

import { defineCustomClientStrategy } from '#lib/paraglide/runtime.js';
import { COOKIE, pageLocale } from '#lib/i18n.ts';

defineCustomClientStrategy('custom-pidlang', {
	getLocale: () => pageLocale(),
	setLocale: (locale) => {
		document.cookie = `${COOKIE}=${locale}; Path=/; Max-Age=31536000; SameSite=Lax`;
		document.documentElement.lang = locale;
	}
});
