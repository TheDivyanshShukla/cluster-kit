// Pure job helpers: roles, progress, merging live updates, and the new-job form check.
import { joinReducer, parseItems, checkItems, type PolicyId } from './sched';
import { NewJob, PRIORITIES, type Cluster, type JobRow, type Role } from './api/schema';

const RANK: Record<Role, number> = { viewer: 0, researcher: 1, operator: 2, admin: 3 };
export const atLeast = (role: Role | null, min: Role) => role != null && RANK[role] >= RANK[min];

export const isActive = (s: JobRow['state']) => s === 'queued' || s === 'running';

/** Every running job of a cluster reply; an older head only sends `job`. */
export const runningJobs = (c: Cluster | null): JobRow[] => (c ? (c.jobs.length ? c.jobs : c.job ? [c.job] : []) : []);

export function progress(j: JobRow): number {
	if (j.state === 'done') return 1;
	return j.total > 0 ? Math.min(1, j.done / j.total) : 0;
}

/** Elapsed run time; `now` is the head's clock so a skewed browser clock cannot show negative times. */
export function elapsedMs(j: JobRow, now: number): number | null {
	if (j.started_ms == null) return null;
	return Math.max(0, (j.finished_ms ?? now) - j.started_ms);
}

export function jobRate(j: JobRow, now: number): number | null {
	const e = elapsedMs(j, now);
	return e == null || e < 1000 || j.done <= 0 ? null : j.done / (e / 1000);
}

export function etaMs(j: JobRow, now: number): number | null {
	const r = j.state === 'running' ? jobRate(j, now) : null;
	return r ? ((j.total - j.done) / r) * 1000 : null;
}

/** Researchers cancel their own jobs, operators any (docs/API.md). */
export function canCancel(role: Role | null, user: string | null, j: JobRow): boolean {
	return isActive(j.state) && (atLeast(role, 'operator') || (atLeast(role, 'researcher') && user === j.submitter));
}

/** Loaded rows plus live updates, newest first. Live rows older than the loaded window stay out. */
export function mergeRows<T extends { id: number }>(items: T[], live: T[]): T[] {
	const lo = items.length ? Math.min(...items.map((i) => i.id)) : -Infinity;
	const byId = new Map(items.map((i) => [i.id, i]));
	for (const l of live) if (l.id >= lo) byId.set(l.id, l);
	return [...byId.values()].sort((a, b) => b.id - a.id);
}

export interface JobForm {
	kind: 'sha' | 'cmd' | 'synth' | 'list';
	name: string;
	from: number | null;
	n: number | null;
	chunk: number | null;
	sha: string;
	cmd: string;
	synth_ns: number | null;
	selector: string;
	priority: (typeof PRIORITIES)[number];
	policy: PolicyId;
	reducerId: string;
	reducerN: number | null;
	itemsText: string;
	max_pcs: number | null;
	placement: '' | 'spread' | 'pack' | 'fastest' | 'efficient';
	mem_mb: number | null;
}

export const emptyForm = (): JobForm => ({ kind: 'synth', name: '', from: 1, n: null, chunk: 0, sha: '', cmd: '', synth_ns: 1000, selector: '', priority: 'normal', policy: 'adaptive', reducerId: '', reducerN: 5, itemsText: '', max_pcs: null, placement: '', mem_mb: null });

const FRIENDLY: Record<string, string> = {
	n: 'A whole number above 0.',
	from: 'A whole number, 0 or more.',
	chunk: 'A whole number, 0 or more. 0 lets the head choose.',
	sha: 'Hex digits only, up to 64 characters.',
	synth_ns: 'A whole number above 0.',
	name: 'At most 120 characters.',
	selector: 'At most 400 characters.',
	reducer: 'At most 20 characters.'
};

/** Checks the form with the NewJob schema. Returns the request body, or one message per field. */
export function validateJob(f: JobForm): { value?: NewJob; errors: Record<string, string> } {
	const errors: Record<string, string> = {};
	if (f.kind !== 'list' && f.n == null) errors.n = 'Enter how many numbers to cover.';
	if (f.kind === 'sha' && !f.sha.trim()) errors.sha = 'Enter the hash to look for.';
	if ((f.kind === 'cmd' || f.kind === 'list') && !f.cmd.trim()) errors.cmd = 'Enter the command to run.';
	if (f.kind === 'synth' && f.synth_ns == null) errors.synth_ns = 'Enter the work per number in ns.';
	const items = f.kind === 'list' ? parseItems(f.itemsText) : [];
	if (f.kind === 'list') {
		const bad = checkItems(items);
		if (bad) errors.items = bad;
	}
	for (const k of ['max_pcs', 'mem_mb'] as const) if (f[k] != null && !(Number.isInteger(f[k]) && f[k]! >= 0)) errors[k] = 'A whole number, 0 or more.';
	const red = joinReducer(f.reducerId, f.reducerN);
	if (red.error) errors.reducer = red.error;
	if (Object.keys(errors).length) return { errors };
	const body = {
		kind: f.kind,
		name: f.name.trim() || undefined,
		from: f.from ?? 1,
		n: f.kind === 'list' ? items.length : f.n!,
		chunk: f.chunk ?? 0,
		sha: f.kind === 'sha' ? f.sha.trim() : undefined,
		cmd: f.kind === 'cmd' || f.kind === 'list' ? f.cmd.trim() : undefined,
		items: f.kind === 'list' ? items : undefined,
		policy: f.policy,
		reducer: red.value || undefined,
		synth_ns: f.kind === 'synth' ? f.synth_ns! : undefined,
		selector: f.selector.trim() || undefined,
		priority: f.priority,
		max_pcs: f.max_pcs || undefined,
		placement: f.placement || undefined,
		mem_mb: f.mem_mb || undefined
	};
	const p = NewJob.safeParse(body);
	if (p.success) return { value: body, errors };
	for (const i of p.error.issues) {
		const k = String(i.path[0] ?? 'form');
		errors[k] ??= FRIENDLY[k] ?? i.message;
	}
	return { errors };
}
