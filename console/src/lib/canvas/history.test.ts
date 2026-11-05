import { expect, test } from 'bun:test';
import { bucket, History, SPAN_MS } from './history';

const w = (id: string, chunks: number, busy = true) => ({ node_id: id, id: 1, chunks, busy });

test('tasks per second come from chunk-count increases', () => {
	const h = new History();
	h.push([w('a', 10), w('b', 5)], 1000);
	h.push([w('a', 40), w('b', 15)], 2000);
	expect(h.samples[1].tasks).toBe(40);
	expect(h.samples[1].busy).toBe(2);
});
test('samples older than 30 minutes fall off', () => {
	const h = new History();
	h.push([w('a', 0)], 0);
	h.push([w('a', 1)], SPAN_MS + 5000);
	expect(h.samples.length).toBe(1);
});
test('bucket averages and carries empty buckets forward', () => {
	const h = new History();
	for (let i = 0; i < 10; i++) h.push([w('a', i * 10)], i * 1000);
	const b = bucket(h.samples, 0, 20_000, 4);
	expect(b.tasks[0]).toBeGreaterThan(0);
	expect(b.tasks[3]).toBe(b.tasks[2]);
});
