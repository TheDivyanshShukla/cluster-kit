import { expect, test } from 'bun:test';
import type { EventRow, LabRow, NodeRow } from './api/schema';
import { clock, fleetWords, labPills, nextCandidate, pillWords, powerCounts, promotedBetween, recruitSlots, recruitWords, stageOf, standbyStatus, standbyViews, tickerLines, wakeView } from './topology';

const node = (o: Partial<NodeRow> = {}): NodeRow => ({
	node_id: 'n1', host: 'pc-01', addr: 'a', inventory: null, first_seen_ms: 0, last_seen_ms: 0, state: 'active', state_reason: '',
	state_by: '', lab_id: null, x: null, y: null, online: true, labels: {}, pools: [], class: '', calib_mhs: null, presence: 'idle', power: 'awake', ...o
});
const lab = (id: number, name: string): LabRow => ({ id, name, w: 4, h: 4, created_ms: 0 });

test('fleet counts and words', () => {
	const c = powerCounts([node(), node(), node({ power: 'asleep' }), node({ power: 'waking' }), node({ power: 'asleep', state: 'retired' })]);
	expect(c).toEqual({ awake: 2, asleep: 1, waking: 1 });
	expect(fleetWords(c)).toBe('2 awake · 1 asleep · 1 waking');
	expect(fleetWords({ awake: 5, asleep: 0, waking: 0 })).toBe('5 awake');
});

test('waking label counts seconds and gives up after 120 s', () => {
	expect(clock(23)).toBe('0:23');
	expect(wakeView(23)).toEqual({ failed: false, label: 'Waking · 0:23', hint: '' });
	expect(wakeView(119).failed).toBe(false);
	const f = wakeView(120);
	expect(f.failed).toBe(true);
	expect(f.label).toBe('Did not wake');
	expect(f.hint).toContain('Wake-on-LAN');
});

test('standby views look up host and lab, falling back to the id', () => {
	const nodes = { a: node({ node_id: 'a', host: 'lab-a-07', lab_id: 1 }) };
	const v = standbyViews([{ node_id: 'a', acked_seq: 5, lag: 0 }, { node_id: 'zz', acked_seq: 3, lag: 2 }], nodes, [lab(1, 'Lab A')]);
	expect(v.map((s) => [s.n, s.host, s.lab, s.lag])).toEqual([[1, 'lab-a-07', 'Lab A', 0], [2, 'zz', null, 2]]);
});

test('recruit slots, next candidate and wording', () => {
	expect(recruitSlots(2, 1)).toBe(1);
	expect(recruitSlots(1, 3)).toBe(0);
	expect(recruitSlots(null, 0)).toBe(0);
	const nodes = {
		a: node({ node_id: 'a', host: 'taken' }), b: node({ node_id: 'b', host: 'asleep', power: 'asleep', online: false }),
		c: node({ node_id: 'c', host: 'pc-09' }), d: node({ node_id: 'd', host: 'pc-10' })
	};
	expect(nextCandidate(['a', 'b', 'c', 'd'], nodes, ['a'])?.host).toBe('pc-09');
	expect(nextCandidate(['a'], nodes, ['a'])).toBeNull();
	expect(nextCandidate(null, nodes, [])).toBeNull();
	expect(recruitWords('pc-09', '!virtual')).toBe('recruiting… pc-09');
	expect(recruitWords(null, '!virtual')).toBe('no PC can be a standby (selector !virtual)');
});

test('lab pills count PCs and asleep, and flag labs with no standby', () => {
	const nodes = [node({ node_id: 'a', lab_id: 1 }), node({ node_id: 'b', lab_id: 1, power: 'asleep' }), node({ node_id: 'c', lab_id: 2 })];
	const sb = standbyViews([{ node_id: 'a', acked_seq: 1, lag: 0 }], Object.fromEntries(nodes.map((n) => [n.node_id, n])), [lab(1, 'Lab A'), lab(2, 'Lab B')]);
	const p = labPills(nodes, [lab(1, 'Lab A'), lab(2, 'Lab B')], sb);
	expect(p.map(pillWords)).toEqual(['Lab A 2 · 1 asleep', 'Lab B 1']);
	expect(p.map((x) => x.noStandby)).toEqual([false, true]);
});

test('failover stage and promotion detection', () => {
	expect(stageOf('primary', false)).toBe('steady');
	expect(stageOf('candidate', false)).toBe('lost');
	expect(stageOf('candidate', true)).toBe('handover');
	expect(stageOf(undefined, false)).toBe('steady');
	expect(promotedBetween(3, 4)).toBe(true);
	expect(promotedBetween(4, 4)).toBe(false);
	expect(promotedBetween(null, 4)).toBe(false);
	const s = standbyViews([{ node_id: 'a', acked_seq: 1, lag: 2 }], {}, [])[0];
	expect(standbyStatus(s, 'lost', false)).toBe('taking over');
	expect(standbyStatus({ ...s, n: 2 }, 'lost', false)).toBe('waiting');
	expect(standbyStatus(s, 'steady', true)).toBe('joined, syncing');
	expect(standbyStatus(s, 'steady', false)).toBe('2 changes behind');
	expect(standbyStatus({ ...s, lag: 0 }, 'steady', true)).toBe('in sync');
});

test('ticker keeps the last 3 ha and power events, newest first, fading', () => {
	const ev = (id: number, kind: string, ts_ms: number): EventRow => ({ id, ts_ms, kind, actor: '', detail: `${kind} #${id}` });
	const now = 1_000_000;
	const lines = tickerLines([ev(1, 'job.done', now - 1), ev(2, 'ha.recruit', now - 5000), ev(3, 'power.sleep', now - 1000), ev(4, 'ha.promote', now - 9000), ev(5, 'power.wake', now - 20000), ev(6, 'power.sleep', now - 700_000)], now);
	expect(lines.map((l) => l.id)).toEqual([3, 2, 4]);
	expect(lines[0].fade).toBe(1);
	expect(lines[2].fade).toBeLessThan(lines[1].fade);
});
