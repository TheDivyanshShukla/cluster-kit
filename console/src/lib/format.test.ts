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
	expect(duration(47 * 3_600_000)).toBe('47 h 0 m');
	expect(duration(40 * 86_400_000 + 3_600_000)).toBe('40 d 1 h');
	expect(ago(10_000, 10_000)).toBe('just now');
	expect(ago(0, 90_000)).toBe('1 m 30 s ago');
});

import { identifying, mbps } from './format';
test('identifying and mbps', () => {
	expect(identifying({ identify_until_ms: 2000 }, 1000)).toBe(true);
	expect(identifying({ identify_until_ms: null }, 1000)).toBe(false);
	expect(identifying({}, 1000)).toBe(false);
	expect(mbps(940)).toBe('940 Mbps');
	expect(mbps(2500)).toBe('2.5 Gbps');
	expect(mbps(null)).toBe('—');
});
import { fileSize } from './format';
test('fileSize', () => {
	expect(fileSize(900)).toBe('900 B');
	expect(fileSize(75_776)).toBe('74 KiB');
	expect(fileSize(5 * 2 ** 20)).toBe('5 MiB');
});
