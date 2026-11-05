import { expect, test } from 'bun:test';
import type { ReservationRow } from './api/schema';
import { checkItems, daysText, estimateText, etaLeft, joinReducer, parseDays, parseHHMM, parseItems, parseLocal, roughSpan, sortBookings, splitReducer, validateBooking, validateWindow, windowSegments, REDUCER_OPTIONS } from './sched';

test('HH:MM parsing matches the head', () => {
	for (const [s, w] of [['09:30', 570], ['0:00', 0], ['23:59', 1439], ['24:00', null], ['12:60', null], ['9', null], ['12:5', null], ['', null], ['ab:cd', null]] as const) expect(parseHHMM(s)).toBe(w);
});

test('days: ranges, wrap, text round trip', () => {
	expect(parseDays('mon-fri')).toEqual([0, 1, 2, 3, 4]);
	expect(parseDays('sat,sun')).toEqual([5, 6]);
	expect(parseDays('fri-mon')).toEqual([0, 4, 5, 6]);
	expect(parseDays('funday')).toBeNull();
	expect(daysText([0, 1, 2, 3, 4])).toBe('mon-fri');
	expect(daysText([0, 2, 5, 6])).toBe('mon,wed,sat,sun');
	expect(daysText([])).toBe('');
});

test('timetable layout: plain and overnight windows', () => {
	expect(windowSegments({ days: 'mon-wed', start: '09:00', end: '17:00' })).toEqual([{ day: 0, from: 540, to: 1020 }, { day: 1, from: 540, to: 1020 }, { day: 2, from: 540, to: 1020 }]);
	expect(windowSegments({ days: 'fri,sun', start: '22:00', end: '06:00' })).toEqual([
		{ day: 4, from: 1320, to: 1440 }, { day: 5, from: 0, to: 360 }, { day: 6, from: 1320, to: 1440 }, { day: 0, from: 0, to: 360 }
	]);
	expect(windowSegments({ days: 'mon', start: '20:00', end: '00:00' })).toEqual([{ day: 0, from: 1200, to: 1440 }]);
	expect(windowSegments({ days: 'mon', start: '10:00', end: '10:00' })).toEqual([]);
	expect(windowSegments({ days: 'zzz', start: '10:00', end: '11:00' })).toEqual([]);
});

test('window form validation', () => {
	const f = { selector: ' lab=A ', days: new Set([0, 1, 2, 3, 4]), start: '9:00', end: '17:00', action: 'cap' as const, cap: 2, note: '' };
	expect(validateWindow(f).value).toEqual({ selector: 'lab=A', days: 'mon-fri', start: '09:00', end: '17:00', action: 'cap', cap_threads: 2 });
	expect(validateWindow({ ...f, days: new Set(), start: '25:00', cap: 0 }).errors).toEqual({ days: expect.any(String), start: expect.any(String), cap: expect.any(String) });
	expect(validateWindow({ ...f, end: '09:00' }).errors.end).toBeDefined();
});

test('local date and booking validation', () => {
	expect(parseLocal('2026-11-05', '18:30')).toBe(new Date(2026, 10, 5, 18, 30).getTime());
	expect(parseLocal('2026-02-30', '10:00')).toBeNull();
	expect(parseLocal('05-11-2026', '10:00')).toBeNull();
	const now = new Date(2026, 10, 4).getTime();
	const b = { selector: '', startDate: '2026-11-05', startTime: '09:00', endDate: '2026-11-05', endTime: '12:00', note: '' };
	expect(validateBooking(b, now).value?.end_ms).toBe(new Date(2026, 10, 5, 12).getTime());
	expect(validateBooking({ ...b, endTime: '08:00' }, now).errors.end).toBeDefined();
	expect(validateBooking({ ...b, startDate: 'x' }, now).errors.start).toBeDefined();
});

test('bookings: current first, then by start', () => {
	const r = (id: number, start_ms: number, active_now = false): ReservationRow => ({ id, selector: '', start_ms, end_ms: start_ms + 1, note: '', active_now });
	expect(sortBookings([r(1, 300), r(2, 100), r(3, 900, true)]).map((x) => x.id)).toEqual([3, 2, 1]);
});

test('estimate wording', () => {
	expect(estimateText(null)).toBe('Estimating…');
	expect(estimateText(null, true)).toContain('No estimate');
	expect(estimateText({ matching_pcs: 23, fleet_rate: 1, eta_secs: 240 })).toBe('About 4 min on 23 PCs.');
	expect(estimateText({ matching_pcs: 1, fleet_rate: 1, eta_secs: 30 })).toBe('About 30 s on 1 PC.');
	expect(estimateText({ matching_pcs: 4, fleet_rate: 1, eta_secs: null })).toContain('No estimate yet');
	expect(estimateText({ matching_pcs: 0, fleet_rate: 0, eta_secs: null })).toContain('No PC matches');
	expect(roughSpan(5400)).toBe('1 h 30 min');
	expect(roughSpan(3)).toBe('a few seconds');
	expect(etaLeft(1_180_000, 1_000_000)).toBe('about 3 min left');
	expect(etaLeft(1_001_000, 1_000_000)).toBe('finishing');
	expect(etaLeft(null, 0)).toBeNull();
});

test('reducers: options, top:N', () => {
	expect(REDUCER_OPTIONS.map((o) => o.value)).toContain('top');
	expect(splitReducer('top:5')).toEqual({ id: 'top', n: 5 });
	expect(splitReducer('sum')).toEqual({ id: 'sum', n: null });
	expect(joinReducer('top', 5).value).toBe('top:5');
	expect(joinReducer('top', 0).error).toBeDefined();
	expect(joinReducer('top', null).error).toBeDefined();
	expect(joinReducer('sum', null).value).toBe('sum');
});

test('list items', () => {
	expect(parseItems('a\r\n\nb \n  \nc')).toEqual(['a', 'b ', 'c']);
	expect(checkItems([])).toBeDefined();
	expect(checkItems(['x'])).toBeNull();
	expect(checkItems(['x'.repeat(4097)])).toContain('Item 1');
});
