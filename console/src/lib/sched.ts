// Pure helpers for time rules, bookings, estimates, policies and reducers (docs/API.md "Smart scheduling").
import type { Estimate, ReservationRow, WindowRow } from './api/schema';

export const DAYS = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'] as const;
export const DAY_LABEL = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'] as const;

/** "09:30" -> minutes since midnight; null when unreadable. Same rule as the head. */
export function parseHHMM(s: string): number | null {
	const m = /^(\d{1,2}):(\d{1,2})$/.exec(s.trim());
	if (!m || m[2].length !== 2) return null;
	const h = Number(m[1]), mi = Number(m[2]);
	return h < 24 && mi < 60 ? h * 60 + mi : null;
}
export const fmtHHMM = (min: number) => `${String(Math.floor(min / 60) % 24).padStart(2, '0')}:${String(min % 60).padStart(2, '0')}`;

/** "mon-fri", "sat,sun", "fri-mon" -> sorted day indexes (Mon = 0); null when unreadable. */
export function parseDays(s: string): number[] | null {
	const idx = (t: string) => DAYS.indexOf(t.trim().toLowerCase() as (typeof DAYS)[number]);
	const out = new Set<number>();
	for (const tok of s.split(',')) {
		const [a, b] = tok.includes('-') ? tok.split('-') : [tok, tok];
		let d = idx(a);
		const e = idx(b);
		if (d < 0 || e < 0) return null;
		for (;;) {
			out.add(d);
			if (d === e) break;
			d = (d + 1) % 7;
		}
	}
	return [...out].sort((x, y) => x - y);
}

/** Day set -> the shortest text the head reads: runs of 3+ become "mon-fri". Empty -> "". */
export function daysText(days: Iterable<number>): string {
	const on = Array.from({ length: 7 }, (_, i) => false);
	for (const d of days) on[d] = true;
	const parts: string[] = [];
	for (let i = 0; i < 7; ) {
		if (!on[i]) { i++; continue; }
		let j = i;
		while (j + 1 < 7 && on[j + 1]) j++;
		if (j - i >= 2) parts.push(`${DAYS[i]}-${DAYS[j]}`);
		else for (let k = i; k <= j; k++) parts.push(DAYS[k]);
		i = j + 1;
	}
	return parts.join(',');
}

export type Seg = { day: number; from: number; to: number };
/** The blocks a window draws on a Mon..Sun x 24 h grid. A window that ends before it starts runs past midnight into the next day. */
export function windowSegments(w: Pick<WindowRow, 'days' | 'start' | 'end'>): Seg[] {
	const days = parseDays(w.days), a = parseHHMM(w.start), b = parseHHMM(w.end);
	if (!days || a == null || b == null || a === b) return [];
	const out: Seg[] = [];
	for (const d of days) {
		if (a < b) out.push({ day: d, from: a, to: b });
		else {
			out.push({ day: d, from: a, to: 1440 });
			if (b > 0) out.push({ day: (d + 1) % 7, from: 0, to: b });
		}
	}
	return out;
}

export const windowText = (w: WindowRow) =>
	`${w.days} ${w.start}-${w.end}: ${w.action === 'block' ? 'no jobs' : `at most ${w.cap_threads ?? '?'} thread${w.cap_threads === 1 ? '' : 's'}`}`;

export interface WindowForm { selector: string; days: Set<number>; start: string; end: string; action: 'block' | 'cap'; cap: number | null; note: string }
export function validateWindow(f: WindowForm): { value?: { selector: string; days: string; start: string; end: string; action: 'block' | 'cap'; cap_threads?: number; note?: string }; errors: Record<string, string> } {
	const errors: Record<string, string> = {};
	if (f.days.size === 0) errors.days = 'Pick at least one day.';
	const a = parseHHMM(f.start), b = parseHHMM(f.end);
	if (a == null) errors.start = 'Use 24-hour HH:MM, like 09:00.';
	if (b == null) errors.end = 'Use 24-hour HH:MM, like 17:00.';
	if (a != null && b != null && a === b) errors.end = 'Start and end must differ. A later end than start is fine: 22:00 to 06:00 runs overnight.';
	if (f.action === 'cap' && !(Number.isInteger(f.cap) && (f.cap as number) >= 1)) errors.cap = 'A whole number of threads, 1 or more.';
	if (Object.keys(errors).length) return { errors };
	return {
		errors,
		value: {
			selector: f.selector.trim(), days: daysText(f.days), start: fmtHHMM(a!), end: fmtHHMM(b!), action: f.action,
			...(f.action === 'cap' ? { cap_threads: f.cap! } : {}), ...(f.note.trim() ? { note: f.note.trim() } : {})
		}
	};
}

/** Local date "2026-11-05" + time "18:00" -> epoch ms in the browser's time zone; null when either is not real. */
export function parseLocal(date: string, time: string): number | null {
	const m = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(date.trim()), t = parseHHMM(time);
	if (!m || t == null) return null;
	const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
	const dt = new Date(y, mo - 1, d, Math.floor(t / 60), t % 60);
	return dt.getFullYear() === y && dt.getMonth() === mo - 1 && dt.getDate() === d ? dt.getTime() : null;
}
export const localDate = (ms: number) => {
	const d = new Date(ms);
	return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};
export const localTime = (ms: number) => fmtHHMM(new Date(ms).getHours() * 60 + new Date(ms).getMinutes());

export function validateBooking(f: { selector: string; startDate: string; startTime: string; endDate: string; endTime: string; note: string }, now: number) {
	const errors: Record<string, string> = {};
	const s = parseLocal(f.startDate, f.startTime), e = parseLocal(f.endDate, f.endTime);
	if (s == null) errors.start = 'Use a date like 2026-11-05 and a time like 09:00.';
	if (e == null) errors.end = 'Use a date like 2026-11-05 and a time like 17:00.';
	if (s != null && e != null) {
		if (e <= s) errors.end = 'The end must be after the start.';
		else if (e <= now) errors.end = 'That booking is already over.';
	}
	if (Object.keys(errors).length) return { errors };
	return { errors, value: { selector: f.selector.trim(), start_ms: s!, end_ms: e!, ...(f.note.trim() ? { note: f.note.trim() } : {}) } };
}

/** Current first, then upcoming by start. */
export const sortBookings = (r: ReservationRow[]) => [...r].sort((a, b) => Number(b.active_now) - Number(a.active_now) || a.start_ms - b.start_ms);

// ---------------------------------------------------------------- estimates, policies, reducers
/** "3 min", "45 s", "1 h 20 min": a rough span for "about ..." sentences. */
export function roughSpan(secs: number): string {
	if (secs < 10) return 'a few seconds';
	if (secs < 60) return `${Math.round(secs / 5) * 5} s`;
	if (secs < 3600) return `${Math.max(1, Math.round(secs / 60))} min`;
	const h = Math.floor(secs / 3600), m = Math.round((secs % 3600) / 60);
	return m === 60 ? `${h + 1} h` : m ? `${h} h ${m} min` : `${h} h`;
}
export function estimateText(e: Estimate | null, failed = false): string {
	if (failed) return 'No estimate available right now.';
	if (!e) return 'Estimating…';
	const pcs = `${e.matching_pcs} PC${e.matching_pcs === 1 ? '' : 's'}`;
	if (e.matching_pcs === 0) return 'No PC matches this selector, so the job would wait.';
	if (e.eta_secs == null) return `${pcs} can take this job. No estimate yet: no speed measured for this kind.`;
	return `About ${roughSpan(e.eta_secs)} on ${pcs}.`;
}
/** Remaining time for a running job, from the head's eta_ms (relative to `now`, the head clock). */
export function etaLeft(eta_ms: number | null, now: number): string | null {
	if (eta_ms == null) return null;
	const left = (eta_ms - now) / 1000;
	return left < 5 ? 'finishing' : `about ${roughSpan(left)} left`;
}

export const POLICIES = [
	{ value: 'adaptive', label: 'Adaptive (recommended)', hint: 'Chunk sizes follow each PC\'s speed, and slow stragglers get a second copy at the end.' },
	{ value: 'guided', label: 'Guided', hint: 'Chunk sizes follow each PC\'s speed, but no second copies at the end.' },
	{ value: 'fixed', label: 'Fixed (paper baseline)', hint: 'Equal chunks for every PC, as in the original paper. Use it to compare.' }
] as const;
export type PolicyId = (typeof POLICIES)[number]['value'];

export const PLACEMENTS = [
	{ value: '', label: 'Spread', hint: 'Use every PC that is free (default).' },
	{ value: 'pack', label: 'Pack', hint: 'Fill PCs already working on this job first, then the biggest.' },
	{ value: 'fastest', label: 'Fastest', hint: 'Give work to the quickest PCs first.' },
	{ value: 'efficient', label: 'Efficient', hint: 'Use the fewest, fastest PCs that still finish in time.' }
] as const;

export const REDUCER_OPTIONS = [
	{ value: '', label: 'Raw parts', hint: 'Parts as they come, no combining.' },
	{ value: 'concat', label: 'Join parts', hint: 'All parts one after another.' },
	{ value: 'lines', label: 'Lines', hint: 'Every output line, in order.' },
	{ value: 'sum', label: 'Sum', hint: 'Add up the numbers found.' },
	{ value: 'count', label: 'Count', hint: 'How many lines there are.' },
	{ value: 'sort', label: 'Sorted lines', hint: 'All lines, sorted.' },
	{ value: 'top', label: 'Top N', hint: 'The N largest numbers.' }
] as const;

/** "top:5" -> {id:'top', n:5}; others -> {id}. */
export function splitReducer(r: string): { id: string; n: number | null } {
	const m = /^top:(\d+)$/.exec(r);
	return m ? { id: 'top', n: Number(m[1]) } : { id: r, n: null };
}
/** Option id + N -> the reducer string; error text when top:N is wrong. */
export function joinReducer(id: string, n: number | null): { value?: string; error?: string } {
	if (id !== 'top') return { value: id };
	return Number.isInteger(n) && (n as number) >= 1 && (n as number) <= 1_000_000 ? { value: `top:${n}` } : { error: 'N is a whole number, 1 or more.' };
}
export const reducerLabel = (r: string) => {
	const { id, n } = splitReducer(r);
	const o = REDUCER_OPTIONS.find((x) => x.value === id);
	return id === 'top' ? `Top ${n}` : (o?.label ?? r);
};

/** Pasted or uploaded text -> items: one per line, blank lines dropped, CR removed. */
export const parseItems = (text: string): string[] => text.split('\n').map((l) => l.replace(/\r$/, '')).filter((l) => l.trim() !== '');
export const MAX_ITEMS = 1_000_000, MAX_ITEM_BYTES = 4096;
export function checkItems(items: string[]): string | null {
	if (items.length === 0) return 'Add at least one item.';
	if (items.length > MAX_ITEMS) return 'At most 1,000,000 items.';
	const i = items.findIndex((s) => new TextEncoder().encode(s).length > MAX_ITEM_BYTES);
	return i >= 0 ? `Item ${i + 1} is longer than 4 KiB.` : null;
}
