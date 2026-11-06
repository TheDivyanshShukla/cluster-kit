import { expect, test } from 'bun:test';
import { announceFor, drain, newLimiter, push, waitMs } from './announce';

test('only meaningful kinds are announced', () => {
	expect(announceFor({ kind: 'job.done', detail: 'job 7 sha' })).toBe('Job finished: job 7 sha');
	expect(announceFor({ kind: 'job.failed', detail: '' })).toBe('Job failed: Job failed');
	expect(announceFor({ kind: 'node.lost', detail: 'pc1' })).toBe('PC lost: pc1');
	expect(announceFor({ kind: 'labhead.partition', detail: 'A' })).toBe('Lab head cut off: A');
	expect(announceFor({ kind: 'job.started', detail: 'x' })).toBeNull();
	expect(announceFor({ kind: 'power.sleep', detail: 'x' })).toBeNull();
});

test('rate limit: first goes out, the rest are joined after the gap', () => {
	const s = newLimiter();
	expect(push(s, 'a', 1000)).toBe('a');
	expect(push(s, 'b', 2000)).toBeNull();
	expect(push(s, 'c', 3000)).toBeNull();
	expect(waitMs(s, 3000)).toBe(2000);
	expect(drain(s, 4999)).toBeNull();
	expect(drain(s, 5000)).toBe('b. And 1 more.');
	expect(drain(s, 9000)).toBeNull();
	expect(push(s, 'd', 9000)).toBe('d');
});
