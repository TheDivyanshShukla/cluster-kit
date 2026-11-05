import { expect, test } from 'bun:test';
import type { Tunable } from './api/schema';
import { groupTunables, invalidKeys, isChanged, patchOf, pending, problem, rangeHint, riskyOf } from './tunables';

const t = (o: Partial<Tunable>): Tunable => ({ key: 'a', group: 'G', value: 5, default: 5, min: 1, max: 10, unit: 's', scope: 'head', help: '', source: 'default', ...o });
const list = [t({ key: 'lost_after_s', group: 'Liveness', help: 'Silence before lost', max: 120 }), t({ key: 'pipeline', group: 'Dispatch', value: 4, unit: 'chunks' }), t({ key: 'ring_max', group: 'Dispatch', source: 'env' })];

test('range hint, validation and changed badge', () => {
	expect(rangeHint(t({ min: 1, max: 100000, unit: 'x' }))).toBe('1 to 100,000 x');
	expect(problem(t({}), '5')).toBeNull();
	expect(problem(t({}), '11')).toBe('Use 1 to 10 s.');
	expect(problem(t({}), '')).toBe('Enter a number.');
	expect(isChanged(list[1])).toBe(true);
	expect(isChanged(list[0])).toBe(false);
});

test('search groups in registry order and matches key words, help and unit', () => {
	expect(groupTunables(list, '').map((g) => g.group)).toEqual(['Liveness', 'Dispatch']);
	expect(groupTunables(list, 'lost silence').map((g) => g.items[0].key)).toEqual(['lost_after_s']);
	expect(groupTunables(list, 'chunks')[0].items[0].key).toBe('pipeline');
	expect(groupTunables(list, 'nothing')).toEqual([]);
});

test('save sends only valid edits that differ; the default goes as null; env is skipped', () => {
	const p = pending(list, { lost_after_s: '8', pipeline: '2', ring_max: '9', a: '1' });
	expect(patchOf(p)).toEqual({ lost_after_s: 8, pipeline: 2 });
	const back = pending(list, { pipeline: '5' });
	expect(patchOf(back)).toEqual({ pipeline: null });
	expect(pending(list, { lost_after_s: '5', pipeline: '4' })).toEqual([]);
	expect(pending(list, { lost_after_s: '999' })).toEqual([]);
	expect(invalidKeys(list, { lost_after_s: '999', pipeline: '3' })).toEqual(['lost_after_s']);
	expect(riskyOf(p)).toEqual(['Liveness']);
	expect(riskyOf(pending(list, { pipeline: '2' }))).toEqual([]);
});
