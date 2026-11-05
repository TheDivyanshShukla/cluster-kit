import { expect, test } from 'bun:test';
import { MAX_PARTICLES, Particles, planParticles } from './particles';

const seq = (() => { let s = 7; return () => ((s = (s * 48271) % 2147483647) / 2147483647); })();

test('a small delta makes a dispatch and a result per chunk', () => {
	const p = planParticles([{ id: 'a', n: 3, job: 1 }], 300, 1000, seq);
	expect(p.length).toBe(6);
	expect(p.filter((x) => x.dir === 'out').length).toBe(3);
});
test('the plan never exceeds the room, however high the task rate', () => {
	const many = Array.from({ length: 1000 }, (_, i) => ({ id: `n${i}`, n: 5, job: null }));
	for (const room of [300, 120, 7, 2]) expect(planParticles(many, room, 1000, seq).length).toBeLessThanOrEqual(room);
	expect(planParticles(many, 1, 1000, seq)).toEqual([]);
	expect(planParticles([{ id: 'a', n: 0, job: null }], 300)).toEqual([]);
});
test('the pool is capped at MAX_PARTICLES and prune frees finished ones', () => {
	const p = new Particles();
	for (let i = 0; i < MAX_PARTICLES + 50; i++) p.spawn(0, 0, 1, 1, 0, 100 + i, null);
	expect(p.n).toBe(MAX_PARTICLES);
	p.prune(149);
	expect(p.n).toBe(MAX_PARTICLES - 50);
	expect(p.room).toBe(50);
});
