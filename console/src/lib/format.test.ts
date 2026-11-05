import { expect, test } from 'bun:test';
import { ago, bytes, count, duration, pct, rate } from './format';

test('missing values are a dash, never zero', () => {
	expect(rate(0)).toBe('—');
	expect(rate(undefined)).toBe('—');
	expect(bytes(null)).toBe('—');
	expect(pct(NaN)).toBe('—');
	expect(count(undefined)).toBe('—');
});

test('units', () => {
	expect(rate(87_380_000)).toBe('87.4 M/s');
	expect(rate(2.2e9)).toBe('2.20 G/s');
	expect(bytes(7.1 * 2 ** 30)).toBe('7.1 GiB');
	expect(pct(96.4)).toBe('96%');
	expect(count(200000)).toBe('2,00,000');
});

test('durations and ages', () => {
	expect(duration(null)).toBe('—');
	expect(duration(42_000)).toBe('42 s');
	expect(duration(125_000)).toBe('2 m 5 s');
	expect(duration(3_900_000)).toBe('1 h 5 m');
	expect(ago(10_000, 10_000)).toBe('just now');
	expect(ago(0, 90_000)).toBe('1 m 30 s ago');
});
