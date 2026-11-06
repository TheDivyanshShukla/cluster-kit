// Pure helpers for the lab-head views (Heads page, Live canvas): wording, progress, validation, ranking.
import type { Candidate, HierLab, HierMode, Hierarchy, HierSettingsPatch } from './api/schema';

export const MODES: { id: HierMode; label: string; what: string }[] = [
	{ id: 'off', label: 'Off', what: 'Every PC works straight for the root. Leases are recalled at once and PCs go back to the root.' },
	{ id: 'manual', label: 'Manual', what: 'Only the lab heads you pin (or start by hand) get leases. Nothing is chosen for you.' },
	{ id: 'auto', label: 'Auto', what: 'The root also picks the best PC of every lab with enough PCs and starts a lab head there.' }
];

/** Settings fields; bounds are the head's tunable limits (rust/src/tune.rs). */
export const FIELDS = [
	{ key: 'lease_s', label: 'Lease length', unit: 's', min: 1, max: 3600, help: 'Seconds of a lab\'s work handed over at once. Shorter loses less when a lab head dies; longer rides out a longer outage.' },
	{ key: 'lab_head_min_pcs', label: 'PCs for a lab head (auto)', unit: 'PCs', min: 2, max: 100000, help: 'Auto mode starts a lab head in a lab with at least this many connected PCs.' },
	{ key: 'lab_silence_s', label: 'Silence before takeover', unit: 's', min: 5, max: 3600, help: 'A silent lab head keeps its leases this long so it can finish and upload; then the root takes them back.' }
] as const;
export type FieldKey = (typeof FIELDS)[number]['key'];

/** Error text per out-of-range or non-integer field; empty object = valid. */
export function validate(v: Record<FieldKey, number>): Partial<Record<FieldKey, string>> {
	const e: Partial<Record<FieldKey, string>> = {};
	for (const f of FIELDS) {
		const n = v[f.key];
		if (!Number.isFinite(n)) e[f.key] = 'Enter a number.';
		else if (n < f.min || n > f.max) e[f.key] = `Between ${f.min} and ${f.max} ${f.unit}.`;
		else if (f.unit === 'PCs' && !Number.isInteger(n)) e[f.key] = 'A whole number.';
	}
	return e;
}
/** Only the fields that changed, ready for PUT /hierarchy/settings. */
export function diff(cur: Hierarchy['settings'], v: Record<FieldKey, number>): HierSettingsPatch {
	const out: HierSettingsPatch = {};
	for (const f of FIELDS) if (v[f.key] !== cur[f.key]) out[f.key] = v[f.key];
	return out;
}

/** 0..1 share of the leased numbers the lab's PCs have finished; null when no lease is held. */
export const leaseProgress = (h: { lab_granted: number; lab_done: number } | null | undefined): number | null =>
	!h || h.lab_granted <= 0 ? null : Math.max(0, Math.min(1, h.lab_done / h.lab_granted));

export type LabTone = 'ok' | 'warn' | 'bad' | 'idle';
export const isCut = (l: Pick<HierLab, 'state' | 'head'>) => l.state === 'partitioned' || !!l.head?.partitioned;
/** One badge per lab: what is true of its lab head right now. */
export function labBadge(l: Pick<HierLab, 'state' | 'head' | 'drain'>): { text: string; tone: LabTone } {
	if (isCut(l)) return { text: 'cut off, working on its lease', tone: 'warn' };
	if (l.state === 'released' || l.head?.released) return { text: 'released, PCs back at the root', tone: 'idle' };
	if (l.state === 'absent') return { text: 'lab head not running yet', tone: 'warn' };
	if (l.state === 'none') return { text: 'no lab head', tone: 'idle' };
	return l.drain ? { text: 'draining', tone: 'warn' } : { text: 'active', tone: 'ok' };
}
export const lagText = (s: number | null | undefined): string => (s == null ? 'no report yet' : s < 1 ? 'under 1 s' : s < 60 ? `${Math.round(s)} s` : `${Math.round(s / 60)} min`);
/** A lab head that has not reported for a while is worth a look. */
export const lagTone = (s: number | null | undefined, lease_s: number): LabTone => (s == null ? 'idle' : s > Math.max(5, lease_s / 3) ? 'bad' : s > 2 ? 'warn' : 'ok');
/** Seconds until the root takes a cut-off lab head's leases back, from the orphan list. */
export const takeoverIn = (h: Hierarchy, lab: string): number | null => h.orphans.find((o) => o.lab === lab)?.expires_in_s ?? null;

/** Candidates for a lab's head: that lab's PCs, eligible first, best score first, host name breaking ties. */
export function rankFor(lab: string, c: Candidate[]): Candidate[] {
	return c.filter((x) => x.lab === lab).sort((a, b) => Number(b.eligible) - Number(a.eligible) || b.score - a.score || a.host.localeCompare(b.host));
}

/** What the canvas draws per lab: name -> head state. */
export interface CanvasHead { lab: string; node_id: string; progress: number | null; cut: boolean; drain: boolean; pcs: number }
export function canvasHeads(h: Hierarchy | null): CanvasHead[] {
	if (!h) return [];
	// a cut-off lab head the root no longer sees (head null, state partitioned) stays on the canvas, dashed
	return h.labs.filter((l) => l.head || l.state === 'partitioned').map((l) => ({ lab: l.lab, node_id: l.head?.node_id ?? '', progress: leaseProgress(l.head), cut: isCut(l), drain: l.drain, pcs: l.head?.pcs ?? 0 }));
}
/** "via lab head X" for the Nodes list; names the lab head's host when known. */
export const viaText = (via: string | null | undefined): string => (via ? `via lab head ${via}` : '');
