import { describe, expect, it } from 'vitest';
import { calendarsOf, datesOf, missingYears, rowsOf, yearsOf } from './holidays';

describe('holiday calendars', () => {
	it('reads dates one per line, in order and once each, and keeps what is not one apart', () => {
		expect(datesOf('2026-10-02\n2026-10-01, 2026-10-01\n\n10/03/2026 2026-02-30x')).toEqual({
			dates: ['2026-10-01', '2026-10-02'],
			invalid: ['10/03/2026', '2026-02-30x']
		});
	});

	it('goes round between the API and the editor', () => {
		const api = {
			cn: { time_zone: 'Asia/Shanghai', dates: ['2026-01-01', '2026-01-02'], source: 'gov.cn' }
		};
		const rows = rowsOf(api);
		expect(rows).toEqual([
			{ name: 'cn', timeZone: 'Asia/Shanghai', source: 'gov.cn', dates: '2026-01-01\n2026-01-02' }
		]);
		expect(calendarsOf(rows)).toEqual(api);
		// A row without a name is not sent; what is not a date is, for the API to name.
		expect(calendarsOf([...rows, { name: ' ', timeZone: '', source: '', dates: '' }])).toEqual(api);
		expect(
			calendarsOf([{ name: 'x', timeZone: 'UTC', source: '', dates: 'soon' }]).x.dates
		).toEqual(['soon']);
	});

	it('says which years have dates and which are missing', () => {
		const dates = ['2026-01-01', '2026-10-01', '2027-01-01'];
		expect(yearsOf(dates)).toBe('2026: 2, 2027: 1');
		expect(missingYears(dates, new Date('2026-06-01T00:00:00Z'))).toEqual([]);
		expect(missingYears(['2026-01-01'], new Date('2026-12-02T00:00:00Z'))).toEqual(['2027']);
		expect(missingYears([], new Date('2027-03-01T00:00:00Z'))).toEqual(['2027']);
	});
});
