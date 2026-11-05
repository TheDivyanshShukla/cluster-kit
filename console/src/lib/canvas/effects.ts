// Event to canvas effect. Pure: the engine adds the timing. Hosts in an event's text ("pc-06 woke up, 14:03") find the PC.
import type { EventRow } from '../api/schema';

export type Tone = 'ok' | 'warn' | 'bad' | 'busy' | 'idle';
export type Fx =
	| { type: 'ripple'; tone: Tone; ids: string[]; head?: boolean }
	| { type: 'glyph'; glyph: 'moon' | 'sunrise'; ids: string[] }
	| { type: 'spec'; ids: string[] }
	| { type: 'failover'; ids: string[] }
	| { type: 'recruit'; ids: string[] };

/** node_id or host (lower case) -> node_id */
export type HostIndex = Map<string, string>;
export function hostIndex(nodes: Iterable<{ node_id: string; host: string }>): HostIndex {
	const m: HostIndex = new Map();
	for (const n of nodes) {
		m.set(n.node_id.toLowerCase(), n.node_id);
		m.set(n.host.toLowerCase(), n.node_id);
	}
	return m;
}
/** PCs named in an event, in order of appearance. */
export function idsIn(ev: Pick<EventRow, 'detail' | 'actor'>, hosts: HostIndex): string[] {
	const out: string[] = [];
	for (const t of ev.detail.toLowerCase().match(/[a-z0-9][a-z0-9._/-]*/g) ?? []) {
		const id = hosts.get(t) ?? hosts.get(t.replace(/[._-]+$/, ''));
		if (id && !out.includes(id)) out.push(id);
	}
	return out;
}

export function effectFor(ev: Pick<EventRow, 'kind' | 'detail' | 'actor'>, hosts: HostIndex): Fx | null {
	const ids = idsIn(ev, hosts);
	switch (ev.kind) {
		case 'node.lost': return { type: 'ripple', tone: 'bad', ids };
		case 'node.join': return { type: 'ripple', tone: 'ok', ids };
		case 'power.sleep': return { type: 'glyph', glyph: 'moon', ids };
		case 'power.wake': return { type: 'glyph', glyph: 'sunrise', ids };
		case 'ha.promote': return { type: 'failover', ids: ids.slice(0, 1) };
		case 'ha.recruit': return { type: 'recruit', ids: ids.slice(0, 1) };
		case 'ha.switchover': return { type: 'failover', ids: ids.slice(-1) };
		case 'node.state': case 'node.approve': return { type: 'ripple', tone: 'warn', ids: ids.slice(0, 1) };
		default:
			if (/^(spec|task\.spec|chunk\.spec)/.test(ev.kind)) return { type: 'spec', ids };
			return null;
	}
}

// ---------------------------------------------------------------- the event rail
export type RailKey = 'lost' | 'join' | 'sleep' | 'wake' | 'head' | 'recruit' | 'switch' | 'job' | 'state' | 'user' | 'other';
export function railKind(kind: string): { key: RailKey; tone: Tone } {
	if (kind === 'node.lost') return { key: 'lost', tone: 'bad' };
	if (kind === 'node.join') return { key: 'join', tone: 'ok' };
	if (kind === 'power.sleep') return { key: 'sleep', tone: 'idle' };
	if (kind === 'power.wake') return { key: 'wake', tone: 'warn' };
	if (kind === 'ha.promote') return { key: 'head', tone: 'busy' };
	if (kind === 'ha.recruit') return { key: 'recruit', tone: 'ok' };
	if (kind === 'ha.switchover') return { key: 'switch', tone: 'busy' };
	if (kind.startsWith('job.')) return { key: 'job', tone: kind === 'job.done' ? 'ok' : 'busy' };
	if (kind.startsWith('node.')) return { key: 'state', tone: 'warn' };
	if (kind.startsWith('auth.') || kind.startsWith('user.')) return { key: 'user', tone: 'idle' };
	return { key: 'other', tone: 'idle' };
}
export const railText = (ev: Pick<EventRow, 'kind' | 'detail'>) => ev.detail || ev.kind;

// ---------------------------------------------------------------- timed effects
export type Live = { fx: Fx; id: string | 'head'; t0: number; dur: number };
export const RIPPLE_MS = 1400;
export const MAX_EFFECTS = 80;
/** Expand an effect to one timed instance per PC (a head-wide one when no PC is named), capped. */
export function schedule(list: Live[], fx: Fx, now: number): Live[] {
	const targets = fx.ids.length ? fx.ids : ['head'];
	const next = list.filter((l) => now - l.t0 < l.dur).concat(targets.slice(0, 30).map((id) => ({ fx, id, t0: now, dur: RIPPLE_MS })));
	return next.length > MAX_EFFECTS ? next.slice(next.length - MAX_EFFECTS) : next;
}
