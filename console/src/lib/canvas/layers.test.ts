import { expect, test } from 'bun:test';
import { decodeView, encodeView, heat, isLayer, layerValue, lodOf, HEAT_STEPS } from './layers';

test('heat is quantised and ends at the ramp ends', () => {
	expect(heat(0)).toBe('rgb(68 1 84)');
	expect(heat(1)).toBe('rgb(253 231 37)');
	expect(heat(0.5001)).toBe(heat(0.5));
	const seen = new Set(Array.from({ length: 1000 }, (_, i) => heat(i / 999)));
	expect(seen.size).toBe(HEAT_STEPS);
});
test('layer values: unknown stays null, throughput is relative to the best', () => {
	const m = { cpu: 50, rate: 5, losses: 2, score: null };
	expect(layerValue('load', m, { rate: 10, losses: 0 })).toBe(0.5);
	expect(layerValue('throughput', m, { rate: 10, losses: 0 })).toBe(0.5);
	expect(layerValue('fitness', m, { rate: 10, losses: 0 })).toBeNull();
	expect(layerValue('load', { ...m, cpu: null }, { rate: 1, losses: 0 })).toBeNull();
	expect(layerValue('reliability', m, { rate: 1, losses: 0 })).toBeCloseTo(2 / 3);
});
test('semantic zoom thresholds', () => {
	expect([lodOf(3), lodOf(6), lodOf(40), lodOf(41)]).toEqual(['point', 'square', 'square', 'card']);
});
test('view url round-trip and rejection', () => {
	expect(decodeView(encodeView(1.2345, 10.126, -4))).toEqual({ k: 1.23, cx: 10.13, cy: -4 });
	expect(decodeView('x,1,2')).toBeNull();
	expect(decodeView('0,1,2')).toBeNull();
	expect(decodeView(null)).toBeNull();
	expect(isLayer('load') && !isLayer('nope')).toBe(true);
});
