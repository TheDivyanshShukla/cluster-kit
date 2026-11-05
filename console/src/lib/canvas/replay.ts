// Replay: rebuild what the fleet looked like at an earlier time by undoing the events that came after it, newest first.
// Read-only. Only changes an event names can be undone (lost, joined, sleep, wake, state); busy/idle is not in the log.
import type { EventRow, NodeState } from '../api/schema';
import { idsIn, type HostIndex } from './effects';

export type NodeSnap = { online: boolean; power: 'awake' | 'asleep' | 'waking'; state: NodeState };

export function rewind(current: Record<string, NodeSnap>, events: Pick<EventRow, 'id' | 'ts_ms' | 'kind' | 'detail' | 'actor'>[], t: number, hosts: HostIndex): Record<string, NodeSnap> {
	const out: Record<string, NodeSnap> = {};
	const get = (id: string) => out[id] ?? (current[id] ? { ...current[id] } : undefined);
	const after = events.filter((e) => e.ts_ms > t).sort((a, b) => b.ts_ms - a.ts_ms || b.id - a.id);
	for (const e of after) {
		for (const id of idsIn(e, hosts)) {
			const s = get(id);
			if (!s) continue;
			if (e.kind === 'node.lost') Object.assign(s, { online: true, power: 'awake' });
			else if (e.kind === 'node.join') Object.assign(s, { online: false, power: 'awake' });
			else if (e.kind === 'power.sleep') Object.assign(s, { online: true, power: 'awake' });
			else if (e.kind === 'power.wake') Object.assign(s, { online: false, power: 'asleep' });
			else if (e.kind === 'node.approve') s.state = 'pending';
			else if (e.kind === 'node.state') s.state = /-> *active\b/.test(e.detail) ? 'draining' : 'active';
			else continue;
			out[id] = s;
		}
	}
	return out;
}

/** Host named by the last `ha.promote` at or before t (null when none is known before t). */
export function headAt(events: Pick<EventRow, 'ts_ms' | 'kind' | 'detail' | 'actor' | 'id'>[], t: number, hosts: HostIndex): string | null {
	const p = events.filter((e) => e.kind === 'ha.promote' && e.ts_ms <= t).sort((a, b) => b.ts_ms - a.ts_ms || b.id - a.id)[0];
	return p ? (idsIn(p, hosts)[0] ?? null) : null;
}
