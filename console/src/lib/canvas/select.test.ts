import { expect, test } from 'bun:test';
import { parseSelector } from './select';

const run = (q: string, a: Record<string, string | string[]>) => (parseSelector(q) as (a: Record<string, string | string[]>) => boolean)(a);

test('lab, job, state and prefix terms', () => {
	const a = { lab: 'Lab A', job: '12', state: ['active', 'lost'], host: 'pc-03', threads: '8' };
	expect(run('lab=A', a)).toBe(true);
	expect(run('lab=B', a)).toBe(false);
	expect(run('job=12 state=lost', a)).toBe(true);
	expect(run('host=pc-*', a)).toBe(true);
	expect(run('threads>=16', a)).toBe(false);
	expect(run('!broken', a)).toBe(true);
	expect(run('', a)).toBe(true);
});
test('bad terms return a message', () => {
	expect(typeof parseSelector('a!b=')).toBe('string');
});
