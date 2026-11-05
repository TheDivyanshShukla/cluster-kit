// Pure helpers for the high-availability views (wording and the failover timer).
import type { HaStatus } from './api/schema';

export const roleLabel = (r: HaStatus['role']) => ({ primary: 'Primary', standby: 'Standby', candidate: 'Taking over…' })[r];

/** "in sync" at lag 0, else "3 changes behind". */
export const lagWords = (lag: number) => (lag <= 0 ? 'in sync' : `${lag} change${lag === 1 ? '' : 's'} behind`);

/** Same rule as rust repl::has_quorum: more than half of the known PCs are connected (or none known). */
export const hasQuorum = (connected: number, membership: number) => membership === 0 || connected * 2 > membership;

export function quorumWords(connected: number, membership: number) {
	return hasQuorum(connected, membership)
		? `${connected} of ${membership} PCs connected — enough to work`
		: `${connected} of ${membership} PCs connected — waiting for more PCs before handing out work`;
}

/** The standby's 503 says "...the primary is <addr>"; returns <addr> or null. */
export const primaryAddr = (msg: string) => /primary is\s+(\S+?)\.?\s*$/.exec(msg)?.[1] ?? null;

export const RECOVERED_SHOW_MS = 30_000;
export type Failover = { since: number | null; recovered: { secs: number; until: number } | null };
export const noFailover: Failover = { since: null, recovered: null };

/** Advance the timer state. `down` = the head is gone or a standby is still taking over. */
export function failoverStep(s: Failover, down: boolean, now: number): Failover {
	if (down) return { since: s.since ?? now, recovered: null };
	if (s.since != null) return { since: null, recovered: { secs: Math.round((now - s.since) / 1000), until: now + RECOVERED_SHOW_MS } };
	return s.recovered && now >= s.recovered.until ? noFailover : s;
}

/** What the banner shows at `now`. */
export function failoverView(s: Failover, now: number): { kind: 'down' | 'recovered'; secs: number } | null {
	if (s.since != null) return { kind: 'down', secs: Math.max(0, Math.round((now - s.since) / 1000)) };
	if (s.recovered && now < s.recovered.until) return { kind: 'recovered', secs: s.recovered.secs };
	return null;
}
