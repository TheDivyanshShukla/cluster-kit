import { expect, test } from 'bun:test';
import { failoverStep, failoverView, lagWords, noFailover, primaryAddr, quorumWords } from './ha';

test('lag and quorum wording', () => {
	expect(lagWords(0)).toBe('in sync');
	expect(lagWords(1)).toBe('1 change behind');
	expect(lagWords(3)).toBe('3 changes behind');
	expect(quorumWords(7, 10)).toBe('7 of 10 PCs connected — enough to work');
	expect(quorumWords(5, 10)).toContain('waiting for more PCs');
	expect(quorumWords(0, 0)).toContain('enough to work');
});

test('primary address from the 503 message', () => {
	expect(primaryAddr('this head is a standby; the primary is 10.0.0.5:7878')).toBe('10.0.0.5:7878');
	expect(primaryAddr('nope')).toBeNull();
});

test('failover timer: down, recovered for 30 s, then gone', () => {
	let s = failoverStep(noFailover, false, 0);
	expect(failoverView(s, 0)).toBeNull();
	s = failoverStep(s, true, 1000);
	s = failoverStep(s, true, 5000);
	expect(failoverView(s, 13000)).toEqual({ kind: 'down', secs: 12 });
	s = failoverStep(s, false, 10000);
	expect(failoverView(s, 11000)).toEqual({ kind: 'recovered', secs: 9 });
	expect(failoverView(s, 39999)?.kind).toBe('recovered');
	expect(failoverView(s, 40000)).toBeNull();
	expect(failoverStep(s, false, 41000)).toEqual(noFailover);
});
