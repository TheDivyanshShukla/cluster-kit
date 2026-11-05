import { expect, test } from 'bun:test';
import { actionsFor, fuzzy, parseQuery, rank, selectorOf, type Entry } from './palette';

test('type prefixes and > route to one group; selectors are not prefixes', () => {
	expect(parseQuery('pc: lab 3')).toEqual({ kind: 'pc', text: 'lab 3', selector: null });
	expect(parseQuery('job:12')).toEqual({ kind: 'job', text: '12', selector: null });
	expect(parseQuery('set:lost')).toMatchObject({ kind: 'set', text: 'lost' });
	expect(parseQuery('> drain pc-12')).toEqual({ kind: 'action', text: 'drain pc-12', selector: null });
	expect(parseQuery('foo: bar').kind).toBeNull();
	expect(parseQuery('lab=A state=lost')).toMatchObject({ kind: null, selector: 'lab=A state=lost' });
	expect(parseQuery('pc-12').selector).toBeNull();
});
test('selector detection needs an operator on every term', () => {
	expect(selectorOf('lab=A')).toBe('lab=A');
	expect(selectorOf('cores>=8 !online')).toBeNull();
	expect(selectorOf('lab=A pc-1')).toBeNull();
	expect(selectorOf('')).toBeNull();
});
test('fuzzy: exact > prefix > word start > substring > subsequence > none', () => {
	const s = ['pc-12', 'pc-120', 'lab pc-12', 'xpc-12', 'p c 1 2'].map((t) => fuzzy(t, 'pc-12'));
	expect(s[0]).toBeGreaterThan(s[1]);
	expect(s[1]).toBeGreaterThan(s[2]);
	expect(s[2]).toBeGreaterThan(s[3]);
	expect(fuzzy('lost_after_ms', 'lam')).toBeGreaterThan(0);
	expect(fuzzy('lost_after_ms', 'lam')).toBeLessThan(fuzzy('lost_after_ms', 'after'));
	expect(fuzzy('pc-12', 'zz')).toBe(-1);
	expect(fuzzy('Lab B', 'lab b')).toBe(1000);
});
const E = (id: string, kind: Entry['kind'], label: string, sub = ''): Entry => ({ id, kind, label, sub });
test('rank groups by kind order, best first, and recent items float up', () => {
	const es = [E('j1', 'job', 'pc scan'), E('p2', 'pc', 'pc-02'), E('p12', 'pc', 'pc-12'), E('p1', 'pc', 'pc-01'), E('s', 'set', 'pcs_max')];
	const r = rank(es, parseQuery('pc'));
	expect(r.map((h) => h.e.kind)).toEqual(['pc', 'pc', 'pc', 'job', 'set']);
	expect(r.filter((h) => h.e.kind === 'pc').map((h) => h.e.id)).toEqual(['p1', 'p2', 'p12']);
	const rec = rank(es, parseQuery('pc'), ['p12']);
	expect(rec[0].e.id).toBe('p12');
	expect(rank(es, parseQuery('job: pc')).map((h) => h.e.id)).toEqual(['j1']);
	expect(rank(es, parseQuery('pc'), [], 1).filter((h) => h.e.kind === 'pc')).toHaveLength(1);
});
test('actions resolve PCs and labs, carry the needed role, and never run without act data', () => {
	const c = { nodes: [{ node_id: 'n1', host: 'pc-12', lab_id: 1 }, { node_id: 'n2', host: 'pc-13', lab_id: 1 }, { node_id: 'n3', host: 'pc-20', lab_id: 2 }], labs: [{ id: 1, name: 'Lab B' }, { id: 2, name: 'Lab C' }] };
	const d = actionsFor('drain pc-12', c);
	expect(d[0].act?.run).toEqual({ type: 'state', state: 'draining', ids: ['n1'] });
	expect(d[0].need).toBe('operator');
	const s = actionsFor('sleep lab B', c).find((e) => e.act);
	expect(s?.act?.run).toEqual({ type: 'power', action: 'sleep', ids: ['n1', 'n2'] });
	expect(actionsFor('make pc-20 head', c)[0]).toMatchObject({ need: 'admin', act: { run: { type: 'head', id: 'n3' } } });
	expect(actionsFor('set lost_after_s', c)[0].href).toContain('lost_after_s');
	expect(actionsFor('new job', c).some((e) => e.href === '/jobs/new')).toBe(true);
	expect(actionsFor('drain', c)[0].act).toBeUndefined();
});
test('"lab B" never resolves to Lab A through a stray letter', () => {
	const c = { nodes: [{ node_id: 'n1', host: 'a', lab_id: 1 }, { node_id: 'n2', host: 'b', lab_id: 2 }], labs: [{ id: 1, name: 'Lab A' }, { id: 2, name: 'Lab B' }] };
	expect(actionsFor('wake lab b', c).filter((e) => e.act).map((e) => e.act!.run)).toEqual([{ type: 'power', action: 'wake', ids: ['n2'] }]);
});
