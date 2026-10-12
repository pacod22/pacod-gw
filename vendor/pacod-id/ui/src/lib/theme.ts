// Light, dark, or as the system says: `data-theme` on the root element, remembered in a cookie
// the service reads so the next page opens in it from the first paint.

export type Theme = 'light' | 'dark' | 'system';
const COOKIE = 'pid_theme';

export function currentTheme(): Theme {
	const t = document.documentElement.dataset.theme;
	return t === 'light' || t === 'dark' ? t : 'system';
}

export function setTheme(theme: Theme): void {
	if (theme === 'system') {
		delete document.documentElement.dataset.theme;
		document.cookie = `${COOKIE}=; Path=/; Max-Age=0; SameSite=Lax`;
	} else {
		document.documentElement.dataset.theme = theme;
		document.cookie = `${COOKIE}=${theme}; Path=/; Max-Age=31536000; SameSite=Lax`;
	}
}
