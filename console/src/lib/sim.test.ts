import { expect, test } from 'bun:test';
import { checkScenario, normalizeSim, PRESETS, totalPcs } from './sim';

test('presets are runnable and the caps are enforced', () => {
	for (const p of PRESETS) expect(checkScenario(p.scenario)).toBeNull();
	expect(totalPcs(PRESETS[1]!.scenario)).toBe(10_000);
	expect(checkScenario({ nodes: [{ count: 20_001 }], job: { n: 1 } })).toContain('20,000');
	expect(checkScenario({ day: { hours: 49 }, pcs: [{ count: 1 }] })).toContain('48');
	expect(checkScenario({ nodes: [{ count: 1 }] })).toContain('size');
	expect(checkScenario([])).not.toBeNull();
});
test('a job result in either shape becomes stats and rows', () => {
	const summary = { finished: true, makespan_s: 100, end_s: 100, ideal_s: 90, efficiency_pct: 90, tasks: 500, head_utilisation_pct: 12, node_failures: 3, recovered_work: 7, tasks_per_s: 5, throughput_per_s: 1e6 };
	const a = normalizeSim({ summary, series: [[0, 0, 0, 0, 10, 0], [10, 5, 2, 8, 10, 0.1]] });
	expect(a.kind).toBe('job');
	expect(a.rows[1]).toEqual({ t: 10, done: 5, rate: 2, busy: 8, connected: 10, head: 0.1 });
	expect(a.headline).toContain('90');
	const b = normalizeSim({ ...summary, timeline: [{ t_s: 1, rate_per_s: 4 }] });
	expect(b.rows[0]).toEqual({ t: 1, rate: 4 });
	expect(normalizeSim(null).rows).toEqual([]);
});
test('a day result reads the CSV timeline and the job rows', () => {
	const v = normalizeSim({ kwh: 12.5, hours: 24, wakes: 3, jobs: [{ name: 'a', wait_s: 5, makespan_s: 60, peak_pcs: 9 }], _timeline: 't_s,asleep,booting,awake_idle,busy,class,power_w\n0,1,2,3,4,5,600\n' });
	expect(v.kind).toBe('day');
	expect(v.rows[0]).toEqual({ t: 0, asleep: 1, booting: 2, idle: 3, busy: 4, class: 5, watts: 600 });
	expect(v.jobs[0]!.peak_pcs).toBe(9);
});
