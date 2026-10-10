/** The currency costs are counted in, for components that show summed costs. */
import { page } from '$app/state';

/**
 * The installation's reporting currency, from the session: reports, the overview and spend
 * limits count costs in it. Empty outside the logged-in pages.
 */
export function reportingCurrency(): string {
	const principal = page.data.principal as { reporting_currency?: string } | null | undefined;
	return principal?.reporting_currency ?? '';
}
