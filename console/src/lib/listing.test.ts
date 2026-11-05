import { describe, expect, test } from 'bun:test';
import {
	emptyState, facetOptions, filterRows, fromParams, isFiltered, kindGroup, naturalCompare, nextSort, rangesOf, runListing,
	sortRows, splitMatches, toggleFacet, toParams, windowRange, type Spec
} from './listing';

type R = { host: string; state: string; lab: string; cpu: number | null; tags: string[] };
const rows: R[] = [
	{ host: 'pc-10', state: 'active', lab: 'A', cpu: 5, tags: ['x'] },
	{ host: 'pc-2', state: 'lost', lab: 'B', cpu: null, tags: [] },
	{ host: 'pc-1', state: 'active', lab: 'B', cpu: 90, tags: ['x', 'y'] },
	{ host: 'PC-3', state: 'pending', lab: 'A', cpu: 40, tags: ['y'] }
];
const spec: Spec<R> = {
	facets: [
		{ key: 'state', label: 'State', get: (r) => r.state },
		{ key: 'lab', label: 'Lab', get: (r) => r.lab },
		{ key: 'tag', label: 'Tag', get: (r) => r.tags },
		{ key: 'range', label: 'Range', get: () => ['15m', '1h'], single: true }
	],
	text: (r) => [r.host, r.state],
	sorters: { host: (r) => r.host, cpu: (r) => r.cpu },
	base: (a, b) => naturalCompare(a.host, b.host)
};

describe('natural sort', () => {
	test('pc-2 before pc-10, case-insensitive', () => {
		expect(['pc-10', 'pc-2', 'pc-1'].sort(naturalCompare)).toEqual(['pc-1', 'pc-2', 'pc-10']);
		expect(naturalCompare('PC-3', 'pc-3')).toBe(0);
	});
	test('sortRows uses natural order and the default base', () => {
		const s = emptyState();
		expect(sortRows(rows, s, spec).map((r) => r.host)).toEqual(['pc-1', 'pc-2', 'PC-3', 'pc-10']);
		expect(sortRows(rows, { ...s, sort: 'host', dir: 'desc' }, spec).map((r) => r.host)).toEqual(['pc-10', 'PC-3', 'pc-2', 'pc-1']);
	});
	test('empty values stay last in both directions', () => {
		const asc = sortRows(rows, { ...emptyState(), sort: 'cpu', dir: 'asc' }, spec).map((r) => r.host);
		const desc = sortRows(rows, { ...emptyState(), sort: 'cpu', dir: 'desc' }, spec).map((r) => r.host);
		expect(asc).toEqual(['pc-10', 'PC-3', 'pc-1', 'pc-2']);
		expect(desc).toEqual(['pc-1', 'PC-3', 'pc-10', 'pc-2']);
	});
});

describe('filter', () => {
	test('search matches all words in any field', () => {
		expect(filterRows(rows, { ...emptyState(), q: 'pc lost' }, spec).map((r) => r.host)).toEqual(['pc-2']);
		expect(filterRows(rows, { ...emptyState(), q: 'zzz' }, spec)).toEqual([]);
	});
	test('OR inside a facet, AND between facets, array facets', () => {
		const s = { ...emptyState(), f: { state: ['active', 'lost'], lab: ['B'] } };
		expect(filterRows(rows, s, spec).map((r) => r.host)).toEqual(['pc-2', 'pc-1']);
		expect(filterRows(rows, { ...emptyState(), f: { tag: ['y'] } }, spec).map((r) => r.host)).toEqual(['pc-1', 'PC-3']);
	});
	test('counts ignore the facet itself but obey the others', () => {
		const s = { ...emptyState(), f: { lab: ['B'], state: ['active'] } };
		const st = facetOptions(rows, s, spec, 'state');
		expect(st.find((o) => o.value === 'active')).toEqual({ value: 'active', count: 1, active: true });
		expect(st.find((o) => o.value === 'lost')?.count).toBe(1);
		expect(st.find((o) => o.value === 'pending')).toBeUndefined(); // lab B has none
		expect(facetOptions(rows, { ...emptyState(), f: { state: ['retired'] } }, spec, 'state').find((o) => o.value === 'retired')?.count).toBe(0);
	});
	test('toggle, single facet and clear', () => {
		let s = emptyState();
		s = { ...s, f: toggleFacet(s, spec, 'state', 'lost') };
		s = { ...s, f: toggleFacet(s, spec, 'state', 'active') };
		expect(s.f.state).toEqual(['lost', 'active']);
		s = { ...s, f: toggleFacet(s, spec, 'state', 'lost') };
		expect(s.f.state).toEqual(['active']);
		s = { ...s, f: toggleFacet(s, spec, 'range', '15m') };
		s = { ...s, f: toggleFacet(s, spec, 'range', '1h') };
		expect(s.f.range).toEqual(['1h']);
		expect(isFiltered(s)).toBe(true);
		expect(isFiltered(emptyState())).toBe(false);
	});
	test('runListing returns the total for "Showing x of y"', () => {
		const r = runListing(rows, { ...emptyState(), f: { lab: ['A'] } }, spec);
		expect([r.rows.length, r.total]).toEqual([2, 4]);
	});
});

describe('sort toggle', () => {
	test('asc, desc, default', () => {
		let s = emptyState();
		s = { ...s, ...nextSort(s, 'cpu') };
		expect([s.sort, s.dir]).toEqual(['cpu', 'asc']);
		s = { ...s, ...nextSort(s, 'cpu') };
		expect([s.sort, s.dir]).toEqual(['cpu', 'desc']);
		s = { ...s, ...nextSort(s, 'cpu') };
		expect(s.sort).toBe('');
		expect(nextSort({ ...s, sort: 'cpu', dir: 'desc' }, 'host')).toEqual({ sort: 'host', dir: 'asc' });
	});
});

describe('URL round-trip', () => {
	test('state survives toParams -> fromParams, including commas and other params', () => {
		const s = { q: 'rack 7', f: { state: ['lost', 'a,b'], lab: ['B'] }, sort: 'cpu', dir: 'desc' as const };
		const url = toParams(s, spec, new URLSearchParams('tab=details&keep=1'));
		expect(url.get('tab')).toBe('details');
		expect(url.get('sort')).toBe('-cpu');
		const back = fromParams(new URLSearchParams(url.toString()), spec);
		expect(back).toEqual(s);
	});
	test('empty state removes its params; unknown sort is ignored; prefix keeps lists apart', () => {
		const base = toParams({ ...emptyState(), q: 'x' }, spec, new URLSearchParams());
		expect(toParams(emptyState(), spec, base).toString()).toBe('');
		expect(fromParams(new URLSearchParams('sort=nope&state=a'), spec).sort).toBe('');
		const both = toParams({ ...emptyState(), q: 'cand' }, spec, toParams({ ...emptyState(), q: 'tun' }, spec, new URLSearchParams(), 't.'), 'c.');
		expect(fromParams(both, spec, 'c.').q).toBe('cand');
		expect(fromParams(both, spec, 't.').q).toBe('tun');
		expect(fromParams(new URLSearchParams('range=1h,15m'), spec).f.range).toEqual(['1h']);
	});
});

describe('windowing', () => {
	test('visible rows plus buffer, clamped', () => {
		expect(windowRange(0, 400, 40, 10_000, 5)).toEqual({ start: 0, end: 15 });
		expect(windowRange(40_000, 400, 40, 10_000, 5)).toEqual({ start: 995, end: 1015 });
		expect(windowRange(399_800, 400, 40, 10_000, 5).end).toBe(10_000);
		expect(windowRange(0, 400, 40, 0)).toEqual({ start: 0, end: 0 });
	});
	test('a 10,000 row list renders a few dozen rows', () => {
		const w = windowRange(123_456, 700, 40, 10_000);
		expect(w.end - w.start).toBeLessThan(60);
	});
});

describe('helpers', () => {
	test('highlight splits on matches', () => {
		expect(splitMatches('pc-10 lost', 'pc LOST')).toEqual([{ t: 'pc', hit: true }, { t: '-10 ', hit: false }, { t: 'lost', hit: true }]);
		expect(splitMatches('abc', '')).toEqual([{ t: 'abc', hit: false }]);
	});
	test('kind groups and ranges', () => {
		expect(['job.done', 'node.lost', 'ha.promote', 'power.sleep', 'tune.changed', 'auth.login', 'api.request', 'lab.create', 'chunk.requeue'].map(kindGroup)).toEqual(['job', 'node', 'ha', 'power', 'tune', 'auth', 'api', 'other', 'job']);
		expect(rangesOf(1000, 1000 + 20 * 60_000)).toEqual(['1h', '24h']);
	});
});
