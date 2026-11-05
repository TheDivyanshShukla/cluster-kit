// Pure list logic shared by every list in the console (docs/DESIGN.md section 8): search, facet filters with counts,
// sort with natural ordering, URL round-trip and windowing math. No DOM, no Svelte: tested in listing.test.ts.

export type Dir = 'asc' | 'desc';
export interface ListState {
	q: string;
	/** facet key -> chosen values (OR inside a facet, AND between facets) */
	f: Record<string, string[]>;
	/** column key; '' = the list's default order */
	sort: string;
	dir: Dir;
}
export interface Facet<T> {
	key: string;
	label: string;
	/** value(s) of a row for this facet */
	get: (r: T) => string | string[];
	/** at most one chosen value (e.g. a time range) */
	single?: boolean;
	/** display order; other values follow by count */
	order?: string[];
	/** display text of a value */
	text?: (v: string) => string;
}
export interface Spec<T> {
	facets: Facet<T>[];
	/** the fields the search box matches */
	text: (r: T) => (string | number | null | undefined)[];
	/** sortable columns */
	sorters: Record<string, (r: T) => string | number | null | undefined>;
	/** order when no column is chosen */
	base?: (a: T, b: T) => number;
}
export interface Option {
	value: string;
	count: number;
	active: boolean;
}

export const emptyState = (): ListState => ({ q: '', f: {}, sort: '', dir: 'asc' });

const collator = new Intl.Collator('en', { numeric: true, sensitivity: 'base' });
/** pc-2 before pc-10, case-insensitive. */
export const naturalCompare = (a: string, b: string): number => collator.compare(a, b);

type Val = string | number | null | undefined;
/** Numbers numerically, text naturally, empty values always last. */
export function compareValues(a: Val, b: Val): number {
	const ea = a == null || a === '', eb = b == null || b === '';
	if (ea || eb) return ea === eb ? 0 : ea ? 1 : -1;
	if (typeof a === 'number' && typeof b === 'number') return a - b;
	return naturalCompare(String(a), String(b));
}

const arr = (v: string | string[]) => (Array.isArray(v) ? v : [v]);

function matchesQuery<T>(r: T, q: string, spec: Spec<T>): boolean {
	const words = q.toLowerCase().split(/\s+/).filter(Boolean);
	if (!words.length) return true;
	const hay = spec.text(r).filter((x) => x != null).join('\u0001').toLowerCase();
	return words.every((w) => hay.includes(w));
}
function matchesFacets<T>(r: T, f: Record<string, string[]>, spec: Spec<T>, skip?: string): boolean {
	for (const fc of spec.facets) {
		const want = f[fc.key];
		if (fc.key === skip || !want?.length) continue;
		const have = arr(fc.get(r));
		if (!want.some((w) => have.includes(w))) return false;
	}
	return true;
}

export function filterRows<T>(rows: T[], s: ListState, spec: Spec<T>): T[] {
	return rows.filter((r) => matchesQuery(r, s.q, spec) && matchesFacets(r, s.f, spec));
}

/** Chips of one facet. Counts use every other filter (so a chip says what you would get if you chose it). */
export function facetOptions<T>(rows: T[], s: ListState, spec: Spec<T>, key: string): Option[] {
	const fc = spec.facets.find((x) => x.key === key);
	if (!fc) return [];
	const counts = new Map<string, number>();
	for (const r of rows) if (matchesQuery(r, s.q, spec) && matchesFacets(r, s.f, spec, key)) for (const v of new Set(arr(fc.get(r)))) counts.set(v, (counts.get(v) ?? 0) + 1);
	const chosen = s.f[key] ?? [];
	for (const v of chosen) if (!counts.has(v)) counts.set(v, 0);
	const order = fc.order ?? [];
	return [...counts]
		.sort(([a, ca], [b, cb]) => {
			const ia = order.indexOf(a), ib = order.indexOf(b);
			if (ia >= 0 || ib >= 0) return (ia < 0 ? 1e9 : ia) - (ib < 0 ? 1e9 : ib);
			return cb - ca || naturalCompare(a, b);
		})
		.map(([value, count]) => ({ value, count, active: chosen.includes(value) }));
}

export function sortRows<T>(rows: T[], s: ListState, spec: Spec<T>): T[] {
	const get = spec.sorters[s.sort];
	const out = [...rows];
	if (!get) return spec.base ? out.sort(spec.base) : out;
	const sign = s.dir === 'desc' ? -1 : 1;
	// empty values stay last in both directions; ties keep the default order
	return out.sort((a, b) => {
		const x = get(a), y = get(b);
		const ex = x == null || x === '', ey = y == null || y === '';
		if (ex || ey) return ex === ey ? (spec.base?.(a, b) ?? 0) : ex ? 1 : -1;
		return sign * compareValues(x, y) || (spec.base?.(a, b) ?? 0);
	});
}

export interface Result<T> {
	rows: T[];
	total: number;
}
export function runListing<T>(rows: T[], s: ListState, spec: Spec<T>): Result<T> {
	return { rows: sortRows(filterRows(rows, s, spec), s, spec), total: rows.length };
}

/** Anything a person changed from the defaults (search, a chip or a sort)? Drives "Clear filters". */
export const isFiltered = (s: ListState) => s.q.trim() !== '' || Object.values(s.f).some((v) => v.length > 0);
/** Header click: asc, then desc, then back to the default order. */
export function nextSort(s: ListState, col: string): Pick<ListState, 'sort' | 'dir'> {
	if (s.sort !== col) return { sort: col, dir: 'asc' };
	return s.dir === 'asc' ? { sort: col, dir: 'desc' } : { sort: '', dir: 'asc' };
}
export function toggleFacet<T>(s: ListState, spec: Spec<T>, key: string, value: string): Record<string, string[]> {
	const fc = spec.facets.find((x) => x.key === key);
	const cur = s.f[key] ?? [];
	const next = cur.includes(value) ? cur.filter((v) => v !== value) : fc?.single ? [value] : [...cur, value];
	const f = { ...s.f };
	if (next.length) f[key] = next;
	else delete f[key];
	return f;
}

// ---- URL state: ?q=&sort=-cpu&state=lost,pending (prefix keeps two lists on one page apart: ?c.q=)
const RESERVED = ['q', 'sort'];
export function toParams<T>(s: ListState, spec: Spec<T>, base: URLSearchParams, prefix = ''): URLSearchParams {
	const p = new URLSearchParams(base);
	for (const k of [...RESERVED, ...spec.facets.map((f) => f.key)]) p.delete(prefix + k);
	if (s.q.trim()) p.set(prefix + 'q', s.q.trim());
	if (s.sort) p.set(prefix + 'sort', (s.dir === 'desc' ? '-' : '') + s.sort);
	for (const fc of spec.facets) {
		const v = s.f[fc.key];
		if (v?.length) p.set(prefix + fc.key, v.map((x) => x.replaceAll('%', '%25').replaceAll(',', '%2C')).join(','));
	}
	return p;
}
export function fromParams<T>(p: URLSearchParams, spec: Spec<T>, prefix = ''): ListState {
	const s = emptyState();
	s.q = p.get(prefix + 'q') ?? '';
	const sort = p.get(prefix + 'sort') ?? '';
	const desc = sort.startsWith('-');
	const col = desc ? sort.slice(1) : sort;
	if (spec.sorters[col]) ((s.sort = col), (s.dir = desc ? 'desc' : 'asc'));
	for (const fc of spec.facets) {
		const raw = p.get(prefix + fc.key);
		if (!raw) continue;
		const vals = raw.split(',').map((x) => decodeURIComponent(x.replaceAll('%2C', ',').replaceAll('%25', '%'))).filter(Boolean);
		if (vals.length) s.f[fc.key] = fc.single ? vals.slice(0, 1) : vals;
	}
	return s;
}

// ---- windowing: fixed row height, render only what is visible plus a buffer
export const WINDOW_OVER = 500;
export interface Win {
	start: number;
	end: number;
}
export function windowRange(scrollTop: number, viewH: number, rowH: number, total: number, buffer = 10): Win {
	if (total <= 0 || rowH <= 0) return { start: 0, end: 0 };
	const first = Math.floor(Math.max(0, scrollTop) / rowH);
	const visible = Math.ceil(Math.max(viewH, rowH) / rowH);
	return { start: Math.max(0, Math.min(total - 1, first - buffer)), end: Math.min(total, first + visible + buffer) };
}

// ---- search highlighting
export function splitMatches(text: string, q: string): { t: string; hit: boolean }[] {
	const words = [...new Set(q.toLowerCase().split(/\s+/).filter(Boolean))];
	if (!words.length || !text) return [{ t: text, hit: false }];
	const low = text.toLowerCase();
	const mark = new Array<boolean>(text.length).fill(false);
	for (const w of words) for (let i = low.indexOf(w); i >= 0; i = low.indexOf(w, i + w.length)) for (let k = i; k < i + w.length; k++) mark[k] = true;
	const out: { t: string; hit: boolean }[] = [];
	for (let i = 0; i < text.length; ) {
		let j = i;
		while (j < text.length && mark[j] === mark[i]) j++;
		out.push({ t: text.slice(i, j), hit: mark[i]! });
		i = j;
	}
	return out;
}

/** Time ranges as a facet: a row lists every bucket that contains it, so "1h" is a plain single-value filter. */
export const RANGES = [
	{ key: '15m', ms: 15 * 60_000, label: 'Last 15 min' },
	{ key: '1h', ms: 3_600_000, label: 'Last hour' },
	{ key: '24h', ms: 86_400_000, label: 'Last 24 h' }
] as const;
export const rangesOf = (ts: number, now: number): string[] => RANGES.filter((r) => now - ts <= r.ms).map((r) => r.key);
export const rangeLabel = (k: string) => RANGES.find((r) => r.key === k)?.label ?? k;

/** job / node / ha / power / tune / auth / api, from a dotted event or log kind. */
const GROUPS: Record<string, string> = { job: 'job', chunk: 'job', node: 'node', admission: 'node', agent: 'node', pc: 'node', ha: 'ha', standby: 'ha', power: 'power', tune: 'tune', auth: 'auth', user: 'auth', api: 'api' };
export const KIND_GROUPS = ['job', 'node', 'ha', 'power', 'tune', 'auth', 'api', 'other'];
export const kindGroup = (kind: string): string => GROUPS[kind.split(/[._]/)[0]!] ?? 'other';
