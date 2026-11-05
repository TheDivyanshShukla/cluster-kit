// Pure helpers for the All settings list (docs/API.md Tunable): search, validation, what a Save would send.
import type { Tunable } from './api/schema';

/** Changing these can drop PCs, trigger a failover or lock people out, so Save asks first. */
export const RISKY_GROUPS = ['Liveness', 'Head failover', 'Security'];

export const fmtNum = (n: number) => (Number.isFinite(n) ? n.toLocaleString('en-US', { maximumFractionDigits: 3, useGrouping: Math.abs(n) >= 10_000 }) : '—');
export const rangeHint = (t: Pick<Tunable, 'min' | 'max' | 'unit'>) => `${fmtNum(t.min)} to ${fmtNum(t.max)} ${t.unit}`;
export const isChanged = (t: Pick<Tunable, 'value' | 'default'>) => t.value !== t.default;
export const readOnly = (t: Pick<Tunable, 'source'>) => t.source === 'env';

/** null when the text is a number inside the range. */
export function problem(t: Pick<Tunable, 'min' | 'max' | 'unit'>, text: string): string | null {
	const v = Number(text);
	if (text.trim() === '' || !Number.isFinite(v)) return 'Enter a number.';
	return v < t.min || v > t.max ? `Use ${rangeHint(t)}.` : null;
}

/** Groups in registry order; a search matches key, help, group and unit (all words must match). */
export function groupTunables(list: Tunable[], query: string): { group: string; items: Tunable[] }[] {
	const words = query.toLowerCase().split(/\s+/).filter(Boolean);
	const out: { group: string; items: Tunable[] }[] = [];
	for (const t of list) {
		const hay = `${t.key} ${t.key.replaceAll('_', ' ')} ${t.help} ${t.group} ${t.unit}`.toLowerCase();
		if (!words.every((w) => hay.includes(w))) continue;
		(out.find((g) => g.group === t.group) ?? out[out.push({ group: t.group, items: [] }) - 1]).items.push(t);
	}
	return out;
}

export type Pending = { key: string; group: string; from: number; to: number | null; value: number };
/** Edits (key -> text) that differ from the current value and are valid. A value equal to the default is sent as null (reset). */
export function pending(list: Tunable[], draft: Record<string, string>): Pending[] {
	const out: Pending[] = [];
	for (const t of list) {
		const text = draft[t.key];
		if (text === undefined || readOnly(t) || problem(t, text)) continue;
		const v = Number(text);
		if (v === t.value) continue;
		out.push({ key: t.key, group: t.group, from: t.value, value: v, to: v === t.default ? null : v });
	}
	return out;
}
export const patchOf = (p: Pending[]) => Object.fromEntries(p.map((x) => [x.key, x.to]));
export const riskyOf = (p: Pending[]) => [...new Set(p.filter((x) => RISKY_GROUPS.includes(x.group)).map((x) => x.group))];
export const invalidKeys = (list: Tunable[], draft: Record<string, string>) => list.filter((t) => draft[t.key] !== undefined && problem(t, draft[t.key]!)).map((t) => t.key);
