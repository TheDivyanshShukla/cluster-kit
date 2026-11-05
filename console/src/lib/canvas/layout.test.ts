import { expect, test } from 'bun:test';
import { fitView, hitPc, layoutScene, type LabIn, type PcIn } from './layout';

const labs: LabIn[] = [{ id: 1, name: 'A', w: 6, h: 4 }, { id: 2, name: 'B', w: 5, h: 3 }];
const pc = (id: string, lab: number | null = null, x: number | null = null, y: number | null = null): PcIn => ({ id, lab_id: lab, x, y, threads: 8 });

test('placed PCs sit on their seat and never overlap', () => {
	const s = layoutScene([pc('a', 1, 0, 0), pc('b', 1, 1, 0), pc('c', 2, 0, 0), pc('u1'), pc('u2')], labs);
	const keys = new Set(s.pcs.map((p) => `${Math.round(p.x)},${Math.round(p.y)}`));
	expect(keys.size).toBe(5);
	const a = s.pcs[s.index.get('a')!], b = s.pcs[s.index.get('b')!];
	expect(b.x - a.x).toBe(20);
	expect(s.labs.map((l) => l.name)).toEqual(['A', 'B', 'Unassigned']);
});

test('adding a PC or changing nothing but state never moves the others', () => {
	const base = [pc('a', 1, 0, 0), pc('z', 1), pc('m', 1), pc('c', 2, 1, 1), pc('q'), pc('r')];
	const s1 = layoutScene(base, labs);
	const s2 = layoutScene([...base, pc('b', 1), pc('aa')], labs, s1.cells);
	for (const p of s1.pcs) {
		const q = s2.pcs[s2.index.get(p.id)!];
		expect([q.x, q.y]).toEqual([p.x, p.y]);
	}
});

test('input order does not change positions', () => {
	const base = [pc('a', 1), pc('b', 1), pc('c')];
	const pos = (l: PcIn[]) => Object.fromEntries(layoutScene(l, labs).pcs.map((p) => [p.id, [p.x, p.y]]));
	expect(pos([...base].reverse())).toEqual(pos(base));
});

test('a lab with more PCs than seats grows rows; 1000 PCs lay out', () => {
	const many = Array.from({ length: 1000 }, (_, i) => pc(`n${String(i).padStart(4, '0')}`, (i % 10) + 1));
	const big = Array.from({ length: 10 }, (_, i) => ({ id: i + 1, name: `L${i + 1}`, w: 10, h: 10 }));
	const s = layoutScene(many, big);
	expect(s.pcs.length).toBe(1000);
	expect(new Set(s.pcs.map((p) => `${p.x},${p.y}`)).size).toBe(1000);
	const small = layoutScene(Array.from({ length: 30 }, (_, i) => pc(`p${i}`, 2)), labs);
	expect(small.labs.find((l) => l.id === 2)!.h).toBeGreaterThan(5 * 20);
});

test('fitView keeps the scene inside the box; hitPc finds the nearest', () => {
	const s = layoutScene([pc('a', 1, 0, 0), pc('b', 1, 1, 0)], labs);
	const v = fitView(s, 800, 500);
	expect(s.w * v.k).toBeLessThanOrEqual(800);
	expect(s.h * v.k).toBeLessThanOrEqual(500);
	const a = s.pcs[0];
	expect(hitPc(s, a.x + 1, a.y)?.id).toBe('a');
	expect(hitPc(s, a.x + 200, a.y + 200)).toBeNull();
});
