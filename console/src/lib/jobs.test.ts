import { expect, test } from 'bun:test';
import type { JobRow } from './api/schema';
import { atLeast, canCancel, emptyForm, etaMs, jobRate, mergeRows, progress, validateJob } from './jobs';

const job = (o: Partial<JobRow> = {}): JobRow => ({
	id: 1, name: 'j', kind: 'synth', from: 1, end: 101, chunk: 10, total: 100, done: 50, state: 'running',
	submitter: 'ana', created_ms: 0, started_ms: 0, finished_ms: null, error: null, selector: '', priority: 2, policy: '', reducer: '', eta_ms: null, wait_reason: null, files: 0, ...o
});

test('roles and cancel rights', () => {
	expect(atLeast('operator', 'researcher')).toBe(true);
	expect(atLeast('viewer', 'researcher')).toBe(false);
	expect(atLeast(null, 'viewer')).toBe(false);
	expect(canCancel('researcher', 'ana', job())).toBe(true);
	expect(canCancel('researcher', 'bob', job())).toBe(false);
	expect(canCancel('operator', 'bob', job())).toBe(true);
	expect(canCancel('admin', 'ana', job({ state: 'done' }))).toBe(false);
});

test('progress, rate, eta', () => {
	expect(progress(job())).toBe(0.5);
	expect(progress(job({ state: 'done', done: 99 }))).toBe(1);
	expect(progress(job({ total: 0 }))).toBe(0);
	expect(jobRate(job(), 10_000)).toBe(5);
	expect(jobRate(job(), 500)).toBeNull();
	expect(etaMs(job(), 10_000)).toBe(10_000);
	expect(etaMs(job({ state: 'done' }), 10_000)).toBeNull();
});

test('mergeRows: live replaces, newer joins, older than window stays out', () => {
	const a = [job({ id: 5 }), job({ id: 4 })];
	const out = mergeRows(a, [job({ id: 6 }), job({ id: 5, done: 99 }), job({ id: 1 })]);
	expect(out.map((j) => j.id)).toEqual([6, 5, 4]);
	expect(out[1].done).toBe(99);
	expect(mergeRows([], [job({ id: 2 })]).length).toBe(1);
});

test('new job form uses the NewJob schema', () => {
	const f = emptyForm();
	expect(validateJob(f).errors.n).toBeTruthy();
	f.n = 1000;
	expect(validateJob(f).value?.synth_ns).toBe(1000);
	f.kind = 'sha';
	expect(validateJob(f).errors.sha).toBeTruthy();
	f.sha = 'xyz';
	expect(validateJob(f).errors.sha).toContain('Hex');
	f.sha = 'ab12';
	expect(validateJob(f).value).toMatchObject({ kind: 'sha', sha: 'ab12', n: 1000, from: 1 });
	f.n = 1.5;
	expect(validateJob(f).errors.n).toBeTruthy();
});

test('running jobs fall back to the single job; form carries selector and priority', async () => {
	const { runningJobs, validateJob, emptyForm } = await import('./jobs');
	const base = { now_ms: 0, workers: [] };
	expect(runningJobs(null)).toEqual([]);
	expect(runningJobs({ ...base, job: job(), jobs: [] }).length).toBe(1);
	expect(runningJobs({ ...base, job: job(), jobs: [job({ id: 1 }), job({ id: 2 })] }).length).toBe(2);
	const v = validateJob({ ...emptyForm(), n: 10, selector: ' lab=A ', priority: 'urgent' });
	expect(v.value?.selector).toBe('lab=A');
	expect(v.value?.priority).toBe('urgent');
	expect(validateJob({ ...emptyForm(), n: 10 }).value?.selector).toBeUndefined();
});
