// Pure helpers for selectors, priorities, labels, policies and config diffs (docs/API.md "Organisation").
import { PRIORITIES, type CapRow, type ReserveRow, type SelectPreview } from './api/schema';

export const PRIORITY_LABEL = ['Urgent', 'High', 'Normal', 'Low', 'Background'] as const;
export const priorityLabel = (n: number) => PRIORITY_LABEL[n] ?? 'Normal';
/** 0 = urgent .. 4 = background, as the head stores it. */
export const priorityIndex = (name: (typeof PRIORITIES)[number]) => PRIORITIES.indexOf(name);
export const priorityName = (n: number) => PRIORITIES[n] ?? 'normal';

/** Trailing-edge debounce; `cancel` drops a pending call. */
export function debounce<A extends unknown[]>(fn: (...a: A) => void, ms: number) {
	let t: ReturnType<typeof setTimeout> | undefined;
	const d = (...a: A) => {
		clearTimeout(t);
		t = setTimeout(() => fn(...a), ms);
	};
	d.cancel = () => clearTimeout(t);
	return d;
}

export type PreviewState = { kind: 'any' } | { kind: 'wait' } | { kind: 'error'; message: string } | { kind: 'count'; count: number };

/** What the selector bar shows for the text and the latest answer (null = not answered yet). */
export function previewState(text: string, p: SelectPreview | null, failure: string | null = null): PreviewState {
	if (failure) return { kind: 'error', message: failure };
	if (!text.trim()) return { kind: 'any' };
	if (!p) return { kind: 'wait' };
	return p.error ? { kind: 'error', message: p.error } : { kind: 'count', count: p.count };
}
export const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
export function previewText(s: PreviewState, emptyMeans = 'Every PC'): string {
	switch (s.kind) {
		case 'any': return emptyMeans;
		case 'wait': return 'Checking…';
		case 'error': return s.message;
		case 'count': return s.count === 0 ? 'No PC matches' : `${plural(s.count, 'PC')} ${s.count === 1 ? 'matches' : 'match'}`;
	}
}

/** "key=value" (or just "key" for an empty value) -> label. Keys are lower case letters, digits, - _ . */
export function parseLabel(text: string): { key: string; value: string } | { error: string } {
	const [k, ...rest] = text.trim().split('=');
	const key = k.trim().toLowerCase();
	if (!/^[a-z][a-z0-9_.-]{0,31}$/.test(key)) return { error: 'Use a key like room or owner: letters, digits, - _ . (max 32).' };
	return { key, value: rest.join('=').trim() };
}

export const capText = (c: CapRow) =>
	c.max_threads == null ? `${c.selector ? `PCs matching ${c.selector}` : 'All PCs'} use all their threads` : `${c.selector ? `PCs matching ${c.selector}` : 'All PCs'} use at most ${plural(c.max_threads, 'thread')}`;

/** Plain-language reserve, e.g. "Keep 10 PCs in lab=A free". */
export function reserveText(r: ReserveRow): string {
	const parts = [r.keep_n > 0 ? plural(r.keep_n, 'PC') : '', r.keep_pct > 0 ? `${r.keep_pct}%` : ''].filter(Boolean);
	return `Keep ${parts.join(' or ') || 'nothing'} free among ${r.selector ? `PCs matching ${r.selector}` : 'all PCs'}`;
}

export type ChangeKind = 'add' | 'remove' | 'change';
/** Config import preview lines come from the head as text; leading + / - / ~ (or add/remove words) pick the mark. */
export function changeKind(line: string): ChangeKind {
	const l = line.trim().toLowerCase();
	if (/^(\+|add|create|new)/.test(l)) return 'add';
	if (/^(-|remove|delete|drop)/.test(l)) return 'remove';
	return 'change';
}
export const changeText = (line: string) => line.trim().replace(/^[+~-]\s*/, '');
