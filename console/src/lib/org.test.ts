import { expect, test } from 'bun:test';
import { changeKind, changeText, debounce, parseLabel, previewState, previewText, priorityIndex, priorityLabel, priorityName, reserveText } from './org';

test('priority mapping round trips', () => {
	expect(priorityIndex('urgent')).toBe(0);
	expect(priorityIndex('background')).toBe(4);
	expect(priorityName(2)).toBe('normal');
	expect(priorityName(9)).toBe('normal');
	expect(priorityLabel(1)).toBe('High');
});

test('preview state and text', () => {
	const p = (count: number, error: string | null = null) => ({ count, node_ids: [], error });
	expect(previewText(previewState('  ', null))).toBe('Every PC');
	expect(previewText(previewState('', null), 'Any PC')).toBe('Any PC');
	expect(previewText(previewState('lab=A', null))).toBe('Checking…');
	expect(previewText(previewState('lab=A', p(23)))).toBe('23 PCs match');
	expect(previewText(previewState('lab=A', p(1)))).toBe('1 PC matches');
	expect(previewText(previewState('lab=A', p(0)))).toBe('No PC matches');
	expect(previewText(previewState('cores>>8', p(0, 'bad term `cores>>8`')))).toBe('bad term `cores>>8`');
	expect(previewState('x', p(3), 'offline').kind).toBe('error');
});

test('debounce keeps only the last call', async () => {
	const seen: string[] = [];
	const d = debounce((s: string) => seen.push(s), 20);
	d('a');
	d('ab');
	d('abc');
	await Bun.sleep(60);
	expect(seen).toEqual(['abc']);
	d('x');
	d.cancel();
	await Bun.sleep(40);
	expect(seen).toEqual(['abc']);
});

test('label parsing', () => {
	expect(parseLabel(' Room = 12 ')).toEqual({ key: 'room', value: '12' });
	expect(parseLabel('broken')).toEqual({ key: 'broken', value: '' });
	expect(parseLabel('a=b=c')).toEqual({ key: 'a', value: 'b=c' });
	expect('error' in parseLabel('=x')).toBe(true);
	expect('error' in parseLabel('9lives=1')).toBe(true);
});

test('reserve sentence and config diff lines', () => {
	expect(reserveText({ id: 1, selector: 'lab=A', keep_n: 10, keep_pct: 0, note: '', held: [] })).toBe('Keep 10 PCs free among PCs matching lab=A');
	expect(reserveText({ id: 1, selector: '', keep_n: 0, keep_pct: 20, note: '', held: [] })).toBe('Keep 20% free among all PCs');
	expect(changeKind('+ pool night (3 PCs)')).toBe('add');
	expect(changeKind('- cap 4')).toBe('remove');
	expect(changeKind('~ lab A: 6x4 -> 8x4')).toBe('change');
	expect(changeKind('add label room on n001')).toBe('add');
	expect(changeText('+ pool night')).toBe('pool night');
});
