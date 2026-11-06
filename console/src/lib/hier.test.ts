import { expect, test } from 'bun:test';
import { canvasHeads, diff, isCut, labBadge, lagText, lagTone, leaseProgress, rankFor, takeoverIn, validate, viaText } from './hier';
import { Candidate, Hierarchy } from './api/schema';

const tree = (labs: object[], orphans: object[] = [], mode = 'auto') =>
	Hierarchy.parse({ mode, settings: { lease_s: 30, lab_head_min_pcs: 8, lab_silence_s: 90 }, root: { node_id: 'r' }, labs, orphans });
const head = (o: object = {}) => ({ node_id: 'n1', lab_granted: 200, lab_done: 50, pcs: 12, ...o });

test('progress is done over granted, null without a lease', () => {
	expect(leaseProgress({ lab_granted: 200, lab_done: 50 })).toBe(0.25);
	expect(leaseProgress({ lab_granted: 0, lab_done: 0 })).toBeNull();
	expect(leaseProgress(null)).toBeNull();
	expect(leaseProgress({ lab_granted: 10, lab_done: 99 })).toBe(1);
});
test('badges', () => {
	const t = tree([{ lab: 'A', state: 'active', head: head() }, { lab: 'B', state: 'partitioned', head: head() }, { lab: 'C', state: 'absent' }, { lab: 'D', state: 'active', drain: true, head: head() }, { lab: 'E', state: 'active', head: head({ partitioned: true }) }]);
	expect(t.labs.map((l) => labBadge(l).text)).toEqual(['active', 'cut off, working on its lease', 'lab head not running yet', 'draining', 'cut off, working on its lease']);
	expect(isCut(t.labs[4])).toBe(true);
});
test('takeover countdown from orphans', () => {
	const t = tree([], [{ node_id: 'n1', lab: 'B', expires_in_s: 61 }]);
	expect(takeoverIn(t, 'B')).toBe(61);
	expect(takeoverIn(t, 'A')).toBeNull();
});
test('settings validation and diff', () => {
	const ok = { lease_s: 30, lab_head_min_pcs: 8, lab_silence_s: 90 };
	expect(validate(ok)).toEqual({});
	expect(Object.keys(validate({ ...ok, lease_s: 0, lab_head_min_pcs: 2.5, lab_silence_s: NaN })).sort()).toEqual(['lab_head_min_pcs', 'lab_silence_s', 'lease_s']);
	expect(diff({ ...ok, lab_head_standby: false }, { ...ok, lease_s: 60 })).toEqual({ lease_s: 60 });
});
test('lag wording and tone', () => {
	expect(lagText(null)).toBe('no report yet');
	expect(lagText(0.4)).toBe('under 1 s');
	expect(lagText(125)).toBe('2 min');
	expect(lagTone(0.5, 30)).toBe('ok');
	expect(lagTone(4, 30)).toBe('warn');
	expect(lagTone(20, 30)).toBe('bad');
});
test('candidates: this lab only, eligible then score', () => {
	const c = (node_id: string, lab: string, score: number, eligible = true) => Candidate.parse({ node_id, host: node_id, lab, score, parts: { speed: 1, memory: 1, stability: 1, free: 1 }, eligible });
	expect(rankFor('A', [c('x', 'A', 50), c('y', 'B', 99), c('z', 'A', 80, false), c('w', 'A', 70)]).map((x) => x.node_id)).toEqual(['w', 'x', 'z']);
});
test('canvas heads and via text', () => {
	expect(canvasHeads(null)).toEqual([]);
	expect(canvasHeads(tree([{ lab: 'A', state: 'active', head: head() }, { lab: 'B', state: 'none' }]))[0]).toEqual({ lab: 'A', node_id: 'n1', progress: 0.25, cut: false, drain: false, pcs: 12 });
	expect(canvasHeads(tree([{ lab: 'B', state: 'partitioned' }]))[0]).toMatchObject({ lab: 'B', cut: true, progress: null });
	expect(viaText('A')).toBe('via lab head A');
	expect(viaText(null)).toBe('');
});
