import { describe, expect, test } from 'bun:test';
import { karpFlatt, niceTicks, parseCsv, scaling, simSamples, toCsv } from './chart';

describe('chart helpers', () => {
	test('niceTicks', () => expect(niceTicks(0, 64, 4)).toEqual([0, 20, 40, 60]));
	test('csv round trip with quotes', () => {
		const t = toCsv(['a', 'b'], [['x,y', 'say "hi"'], [1, 2]]);
		expect(parseCsv(t)).toEqual([{ a: 'x,y', b: 'say "hi"' }, { a: '1', b: '2' }]);
	});
	test('perfect scaling has efficiency 1 and serial fraction 0', () => {
		const [s] = scaling([1, 2, 4].flatMap((p) => [1, 2].map(() => ({ pcs: p, rate: 100 * p }))));
		expect(s!.pts.map((x) => x.S)).toEqual([1, 2, 4]);
		expect(s!.pts[2]!.E).toBe(1);
		expect(s!.pts[2]!.KF).toBeCloseTo(0);
		expect(Number.isNaN(s!.pts[0]!.KF)).toBe(true);
	});
	test('repeats give a range and Amdahl gives its serial fraction', () => {
		const f = 0.1, S = (p: number) => 1 / (f + (1 - f) / p);
		const [s] = scaling([{ pcs: 1, rate: 100 }, { pcs: 8, rate: 100 * S(8) }, { pcs: 8, rate: 100 * S(8) * 0.9 }]);
		const p8 = s!.pts[1]!;
		expect(p8.Slo).toBeLessThan(p8.Shi);
		expect(karpFlatt(S(8), 8)).toBeCloseTo(f);
		expect(p8.KFlo).toBeLessThan(p8.KFhi);
	});
	test('sim rows in the e1 csv shape', () => {
		expect(simSamples([{ policy: 'fixed', pcs: '2', throughput_per_s: '4e7' }, { pcs: 0 }])).toEqual([{ pcs: 2, rate: 4e7, policy: 'fixed' }]);
	});
});
