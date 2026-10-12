// @pacod/ui: the sign-in and account components every PACOD product shares. Styles read only the
// --pid- tokens of '@pacod/ui/tokens.css'; a plain-CSS app (the gateway console) adds
// '@pacod/ui/console.css', a Tailwind app (PACOD) '@pacod/ui/tailwind.css'.

export { default as AccountMenu } from './AccountMenu.svelte';
export { default as AuthCard } from './AuthCard.svelte';
export { default as BrandMark } from './BrandMark.svelte';
export { default as Button } from './Button.svelte';
export { default as Checkbox } from './Checkbox.svelte';
export { default as Divider } from './Divider.svelte';
export { default as LocaleSwitcher } from './LocaleSwitcher.svelte';
export { default as Notice } from './Notice.svelte';
export { default as PasswordField } from './PasswordField.svelte';
export { default as ProviderButton } from './ProviderButton.svelte';
export { default as TextField } from './TextField.svelte';
export { BRANDS, initials, type Brand } from './brands.js';
