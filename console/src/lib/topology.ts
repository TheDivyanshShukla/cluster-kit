// Pure helpers for the power and head-topology views (docs/DESIGN.md): wording, counts, which standby sits where.
import type { EventRow, HaStatus, LabRow, NodeRow } from './api/schema';
import { lagWords } from './ha';

// ---------------------------------------------------------------- power
export type PowerCounts = { awake: number; asleep: number; waking: number };

export function powerCounts(nodes: Iterable<Pick<NodeRow, 'power' | 'state'>>): PowerCounts {
	const c = { awake: 0, asleep: 0, waking: 0 };
	for (const n of nodes) if (n.state !== 'retired') c[n.power]++;
	return c;
}

/** "142 awake · 58 asleep · 3 waking"; zero asleep or waking parts are left out. */
export function fleetWords(c: PowerCounts): string {
	return [`${c.awake} awake`, c.asleep ? `${c.asleep} asleep` : '', c.waking ? `${c.waking} waking` : ''].filter(Boolean).join(' · ');
}

/** A PC that was sent the magic packet and is still not back after this many seconds did not wake. */
export const WAKE_FAIL_S = 120;
export const clock = (secs: number) => `${Math.floor(secs / 60)}:${String(Math.max(0, Math.floor(secs) % 60)).padStart(2, '0')}`;

export function wakeView(secs: number): { failed: boolean; label: string; hint: string } {
	return secs >= WAKE_FAIL_S
		? { failed: true, label: 'Did not wake', hint: 'Check Wake-on-LAN in the BIOS.' }
		: { failed: false, label: `Waking · ${clock(Math.max(0, secs))}`, hint: '' };
}

// ---------------------------------------------------------------- head and standbys
export type StandbyView = { id: string; host: string; lab: string | null; labId: number | null; lag: number; n: number };

/** Standbys in takeover order with the host and lab looked up from the PC list (the HA feed has only ids). */
export function standbyViews(standbys: HaStatus['standbys'], nodes: Record<string, NodeRow>, labs: LabRow[]): StandbyView[] {
	return standbys.map((s, i) => {
		const n = nodes[s.node_id];
		return { id: s.node_id, host: n?.host ?? s.node_id, lab: labs.find((l) => l.id === n?.lab_id)?.name ?? null, labId: n?.lab_id ?? null, lag: s.lag, n: i + 1 };
	});
}

export const recruitSlots = (target: number | null | undefined, have: number) => (target == null ? 0 : Math.max(0, target - have));

/** The PC the head would recruit next: first selector match that is awake, online and not already a standby. */
export function nextCandidate(ids: string[] | null, nodes: Record<string, NodeRow>, standbyIds: string[]): NodeRow | null {
	const taken = new Set(standbyIds);
	for (const id of ids ?? []) {
		const n = nodes[id];
		if (n && n.online && n.power === 'awake' && n.state === 'active' && !taken.has(id)) return n;
	}
	return null;
}

export const recruitWords = (host: string | null, selector: string) =>
	host ? `recruiting… ${host}` : `no PC can be a standby (selector ${selector.trim() || 'any PC'})`;

export type LabPill = { id: number; name: string; pcs: number; asleep: number; noStandby: boolean };
export function labPills(nodes: Iterable<NodeRow>, labs: LabRow[], standbys: StandbyView[]): LabPill[] {
	const all = [...nodes].filter((n) => n.state !== 'retired');
	const withStandby = new Set(standbys.map((s) => s.labId));
	return labs.map((l) => {
		const mine = all.filter((n) => n.lab_id === l.id);
		return { id: l.id, name: l.name, pcs: mine.length, asleep: mine.filter((n) => n.power === 'asleep').length, noStandby: !withStandby.has(l.id) };
	});
}
export const pillWords = (p: LabPill) => `${p.name} ${p.pcs}${p.asleep ? ` · ${p.asleep} asleep` : ''}`;

export type Stage = 'steady' | 'lost' | 'handover';
/** The head is gone (or being handed over) while the role is candidate; a planned switchover reads "Handing over". */
export function stageOf(role: HaStatus['role'] | undefined, planned: boolean): Stage {
	return role === 'candidate' ? (planned ? 'handover' : 'lost') : 'steady';
}

/** The old head's term has been replaced: the epoch went up while this console watched. */
export const promotedBetween = (prevEpoch: number | null, epoch: number | null) => prevEpoch != null && epoch != null && epoch > prevEpoch;

/** Standby label under a card: "taking over" for #1 while the head is lost, "joined, syncing" for a new one. */
export function standbyStatus(s: StandbyView, stage: Stage, joined: boolean): string {
	if (stage !== 'steady') return s.n === 1 ? 'taking over' : 'waiting';
	return joined && s.lag > 0 ? 'joined, syncing' : lagWords(s.lag);
}

// ---------------------------------------------------------------- wall ticker
const TICKER_KINDS = /^(ha\.|power\.)/;
/** The last `max` HA and power events, newest first, with a fade level 1 (new) .. 0.4 (oldest shown). Events older than 10 min are dropped. */
export function tickerLines(events: EventRow[], now: number, max = 3): { id: number; text: string; fade: number }[] {
	return events
		.filter((e) => TICKER_KINDS.test(e.kind) && now - e.ts_ms < 600_000)
		.sort((a, b) => b.ts_ms - a.ts_ms || b.id - a.id)
		.slice(0, max)
		.map((e, i) => ({ id: e.id, text: e.detail || e.kind, fade: Math.max(0.4, 1 - i * 0.3) }));
}
