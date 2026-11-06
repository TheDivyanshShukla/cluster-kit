// Pure advisor helpers (docs/ADVISOR.md): ordering, labels, the policy table and plain-language change lines.
import type { AdviceApplied, AdviceMode, Suggestion } from './api/schema';

export const SEV_ORDER: Suggestion['severity'][] = ['warn', 'tip', 'info'];
export const SEV_LABEL: Record<Suggestion['severity'], string> = { warn: 'Needs attention', tip: 'Worth doing', info: 'For your information' };

/** Worst first, then longest standing. */
export const adviceOrder = (a: Suggestion, b: Suggestion) => SEV_ORDER.indexOf(a.severity) - SEV_ORDER.indexOf(b.severity) || a.since_ms - b.since_ms;

/** Every kind a policy key can name. `auto` = some option of this kind is reversible and bounded (ADVISOR.md catalogue). */
export const KINDS: { kind: string; label: string; auto: boolean; why: string }[] = [
	{ kind: 'dispatch_wait', label: 'PCs wait for the head', auto: true, why: '' },
	{ kind: 'head_hot', label: 'Head busy', auto: true, why: '' },
	{ kind: 'flaky_pc', label: 'A PC keeps dropping out', auto: true, why: 'Auto only drains the PC; other options need a person.' },
	{ kind: 'slow_pc', label: 'A PC is much slower', auto: true, why: '' },
	{ kind: 'standby_behind', label: 'Standby falling behind', auto: true, why: '' },
	{ kind: 'eco', label: 'Idle PCs, power saving off', auto: true, why: '' },
	{ kind: 'job_fail', label: 'Repeated job failures', auto: true, why: 'Auto only raises output or attempt limits; other causes need a person.' },
	{ kind: 'no_standby', label: 'No standby head', auto: false, why: 'Recruiting a standby copies the database to a PC, so a person should decide.' },
	{ kind: 'reserve_at_risk', label: 'Reserve at risk', auto: false, why: 'The fix is bringing PCs online or approving them, which only a person can do.' },
	{ kind: 'quota_wait', label: 'Job waits on a quota', auto: false, why: 'A quota is a decision about people, not a tuning knob.' },
	{ kind: 'disk', label: 'Head disk filling up', auto: false, why: 'Freeing space deletes old parts or events; that is never automatic.' },
	{ kind: 'blob_peer', label: 'Files sent from the head', auto: false, why: 'The fix is a change to how the job is submitted.' },
	{ kind: 'log_event', label: 'Warning in the logs', auto: false, why: 'These come with manual steps only.' }
];
export const kindLabel = (k: string) => KINDS.find((x) => x.kind === k)?.label ?? k.replace(/_/g, ' ');
export const MODE_LABEL: Record<AdviceMode, string> = { off: 'Off', suggest: 'Suggest', auto: 'Auto' };

/** Kinds to list in the policy table: the known ones plus any the head names that we do not. */
export function policyKinds(policy: Record<string, string>, seen: string[]): { kind: string; label: string; auto: boolean; why: string }[] {
	const extra = [...new Set([...Object.keys(policy), ...seen])].filter((k) => !KINDS.some((x) => x.kind === k));
	return [...KINDS, ...extra.sort().map((kind) => ({ kind, label: kindLabel(kind), auto: false, why: 'Not marked auto-safe.' }))];
}
export const modeOf = (policy: Record<string, string>, kind: string): AdviceMode => (policy[kind] === 'off' || policy[kind] === 'auto' ? policy[kind] : 'suggest');

/** "62 %" for a 0..1 hit rate; "not enough results yet" when the head has none (fewer than 3 judged). */
export const accuracyText = (a: number | null | undefined) => (a == null ? 'not enough results yet' : `${Math.round(a * 100)} % of past predictions held`);

const num = (n: unknown) => (typeof n === 'number' ? (Number.isInteger(n) ? String(n) : String(Math.round(n * 100) / 100)) : String(n ?? ''));
/** One line of a preview or action: "target_chunk_s 2 -> 4", "pc-3: active -> draining". */
export function changeLine(c: { setting?: string; node?: string; from?: unknown; to?: unknown }): string {
	const who = c.setting ?? c.node ?? 'change';
	return `${who}: ${num(c.from)} → ${num(c.to)}`;
}

/** The metric a follow-up judged, as "tail_share 0.4 -> 0.25". */
export const metricLine = (metric: string | null | undefined, from: number, to: number) => (metric ? `${metric.replace(/_/g, ' ')} ${num(from)} → ${num(to)}` : '');

export type Verdict = 'held' | 'missed' | 'waiting' | 'undone';
export function verdict(a: AdviceApplied): Verdict {
	if (a.undone) return 'undone';
	if (!a.outcome || a.outcome.hit == null) return 'waiting';
	return a.outcome.hit ? 'held' : 'missed';
}
export const VERDICT_LABEL: Record<Verdict, string> = { held: 'Held', missed: 'Missed', waiting: 'Waiting for the result', undone: 'Undone' };

/** Open suggestions that need a person: policy not off. */
export const openCount = (list: Suggestion[] | null | undefined) => (list ?? []).filter((s) => s.policy !== 'off').length;

/** Short phrase for nav, links: "1 suggestion" / "3 suggestions". */
export const suggestionsText = (n: number) => `${n} suggestion${n === 1 ? '' : 's'}`;

/** What an action does, in words: "tunable target_chunk_s 2 -> 4", "pc-3 active -> draining". */
export function actionLine(a: { type: string; [k: string]: unknown } | null | undefined): string {
	if (!a) return '';
	const arrow = (from: unknown, to: unknown) => `${num(from)} → ${num(to)}`;
	switch (a.type) {
		case 'tune':
			return `${a.key} ${arrow(a.from, a.to)}`;
		case 'node_state':
			return `${Array.isArray(a.node_ids) ? a.node_ids.join(', ') : ''} ${arrow(a.from, a.to)}`;
		case 'ha_target':
			return `standby heads wanted ${arrow(a.from, a.to)}`;
		case 'power':
			return `power mode ${arrow(a.from, a.to)}`;
		default:
			return '';
	}
}
