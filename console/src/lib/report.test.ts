import { expect, test } from 'bun:test';
import { shiftDay, validDay } from './report';

test('shiftDay crosses month and year ends', () => {
	expect(shiftDay('2026-12-31', 1)).toBe('2027-01-01');
	expect(shiftDay('2026-03-01', -1)).toBe('2026-02-28');
});
test('validDay rejects impossible dates', () => {
	expect(validDay('2026-02-30')).toBe(false);
	expect(validDay('2026-11-05')).toBe(true);
	expect(validDay(null)).toBe(false);
});
