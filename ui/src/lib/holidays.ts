/**
 * The holiday calendars of the platform settings, as the console edits them: dates one per
 * line, and what an administrator should know about them (concept section 13.1). Prices with
 * off-peak hours apply all day on a calendar's dates; countries publish them year by year.
 */

export interface Calendar {
	time_zone: string;
	dates: string[];
	source?: string;
}

/** One calendar in the editor. */
export interface CalendarRow {
	name: string;
	timeZone: string;
	source: string;
	/** One date per line (commas and spaces also separate). */
	dates: string;
}

const DATE = /^\d{4}-\d{2}-\d{2}$/;

/** The calendars as editor rows, by name. */
export function rowsOf(calendars: Record<string, Calendar> | null | undefined): CalendarRow[] {
	return Object.entries(calendars ?? {}).map(([name, c]) => ({
		name,
		timeZone: c.time_zone,
		source: c.source ?? '',
		dates: (c.dates ?? []).join('\n')
	}));
}

/** The dates of a text: in order, once each; what is not a date is listed apart. */
export function datesOf(text: string): { dates: string[]; invalid: string[] } {
	const entries = text
		.split(/[\s,]+/)
		.map((s) => s.trim())
		.filter(Boolean);
	const valid = (d: string) => DATE.test(d) && !Number.isNaN(Date.parse(`${d}T00:00:00Z`));
	return {
		dates: [...new Set(entries.filter(valid))].sort(),
		invalid: entries.filter((d) => !valid(d))
	};
}

/** The calendars as the API takes them. Rows without a name are left out. */
export function calendarsOf(rows: CalendarRow[]): Record<string, Calendar> {
	return Object.fromEntries(
		rows
			.filter((r) => r.name.trim())
			.map((r) => {
				const { dates, invalid } = datesOf(r.dates);
				// What is not a date goes too, so that the API names it in its answer.
				const calendar: Calendar = { time_zone: r.timeZone.trim(), dates: [...dates, ...invalid] };
				if (r.source.trim()) calendar.source = r.source.trim();
				return [r.name.trim(), calendar];
			})
	);
}

/** How many dates each year has: `2026: 33, 2027: 5`. */
export function yearsOf(dates: string[]): string {
	const counts = new Map<string, number>();
	for (const d of dates) counts.set(d.slice(0, 4), (counts.get(d.slice(0, 4)) ?? 0) + 1);
	return [...counts].map(([year, n]) => `${year}: ${n}`).join(', ');
}

/** What is missing, as the gateway's monitoring sees it: no dates for this year, or, from
 * December, none for the next. */
export function missingYears(dates: string[], today: Date): string[] {
	const year = today.getUTCFullYear();
	const has = (y: number) => dates.some((d) => d.startsWith(`${y}-`));
	const missing: string[] = [];
	if (!has(year)) missing.push(String(year));
	if (today.getUTCMonth() === 11 && !has(year + 1)) missing.push(String(year + 1));
	return missing;
}
