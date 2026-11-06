// Screen-reader announcements: only a few meaningful events, rate limited. Never every tick.
import type { EventRow } from './api/schema';
import { railText } from './canvas/effects';

export const GAP_MS = 4000;
export const MAX_AGE_MS = 30_000;
const KEY = 'ck-announce';

/** The sentence for an event worth hearing, or null. */
export function announceFor(ev: Pick<EventRow, 'kind' | 'detail'>): string | null {
	const d = railText(ev);
	switch (ev.kind) {
		case 'job.done':
		case 'job.complete':
			return `Job finished: ${d}`;
		case 'job.failed':
		case 'job.error':
			return `Job failed: ${d}`;
		case 'node.lost':
			return `PC lost: ${d}`;
		case 'node.joined':
			return `PC rejoined: ${d}`;
		case 'ha.promote':
		case 'ha.promoted':
		case 'ha.switchover':
			return `Failover: ${d}`;
		case 'labhead.partition':
			return `Lab head cut off: ${d}`;
		default:
			return null;
	}
}

export type Limiter = { last: number; pending: string[] };
export const newLimiter = (): Limiter => ({ last: -Infinity, pending: [] });

/** Text to say now, or null (queued for `drain`). At most one message per `gap`. */
export function push(s: Limiter, msg: string, now: number, gap = GAP_MS): string | null {
	s.pending.push(msg);
	return drain(s, now, gap);
}
/** Joins what queued up since the last message into one: the first, then a count. */
export function drain(s: Limiter, now: number, gap = GAP_MS): string | null {
	if (!s.pending.length || now - s.last < gap) return null;
	const [first, ...rest] = s.pending;
	s.pending = [];
	s.last = now;
	return rest.length ? `${first}. And ${rest.length} more.` : first!;
}
export const waitMs = (s: Limiter, now: number, gap = GAP_MS) => Math.max(0, s.last + gap - now);

export function loadEnabled(): boolean {
	try {
		return localStorage.getItem(KEY) !== 'off';
	} catch {
		return true;
	}
}
export function saveEnabled(on: boolean) {
	try {
		localStorage.setItem(KEY, on ? 'on' : 'off');
	} catch {}
}
