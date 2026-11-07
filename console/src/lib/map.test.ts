import { expect, test } from 'bun:test';
import type { NodeRow } from './api/schema';
import { autoArrange, boxCells, cellAt, look, tileLabel, toItems, type Item } from './map';

const node = (o: Partial<NodeRow> = {}): NodeRow => ({
	node_id: 'n1', host: 'pc-01', addr: 'a', inventory: null, first_seen_ms: 0, last_seen_ms: 0, state: 'active', state_reason: '',
	state_by: '', lab_id: null, x: null, y: null, online: true, labels: {}, pools: [], class: '', calib_mhs: null, presence: 'idle', power: 'awake', ...o
});
const item = (o: Partial<NodeRow> = {}, w: Item['w'] = null): Item => ({ node: node(o), w });
const wk = (o: object) => ({ busy: false, cpu: 0, ...o }) as Item['w'];

test('state overlay: every state has its own glyph', () => {
	const states = ['pending', 'active', 'cordoned', 'draining', 'maintenance', 'retired'] as const;
	const glyphs = states.map((state) => look(item({ state }), 'state').glyph);
	expect(new Set(glyphs).size).toBe(states.length);
	expect(look(item({ state: 'pending' }), 'state').dash).toBe(true);
});

test('cpu and work overlays', () => {
	expect(look(item({}, wk({ cpu: 91.4 })), 'cpu')).toMatchObject({ glyph: '91', stroke: 'stroke-bad' });
	expect(look(item({}, wk({ cpu: 10 })), 'cpu').stroke).toBe('stroke-idle/50');
	expect(look(item({}, null), 'cpu').glyph).toBe('–');
	expect(look(item({}, wk({ busy: true })), 'work')).toMatchObject({ glyph: '▶', label: 'working' });
	expect(look(item(), 'work').label).toBe('idle');
});

test('offline PCs are dimmed', () => {
	expect(look(item({ online: false }), 'state').dim).toBe(true);
	expect(look(item(), 'state').dim).toBe(false);
});

test('auto-arrange fills empty cells in host order', () => {
	const pcs = ['pc-10', 'pc-2', 'pc-1'].map((host) => item({ host, node_id: host }));
	expect(autoArrange(pcs, 2, 2, ['1,0'])).toEqual([
		{ id: 'pc-1', x: 0, y: 0 },
		{ id: 'pc-2', x: 0, y: 1 },
		{ id: 'pc-10', x: 1, y: 1 }
	]);
	expect(autoArrange(pcs, 1, 1, [])).toHaveLength(1);
});

test('hit-testing at any scale', () => {
	const r = { left: 100, top: 50, width: 200, height: 100 }; // 4x2 lab drawn at 50 px per cell
	expect(cellAt(r, 100, 50, 4, 2)).toEqual({ x: 0, y: 0 });
	expect(cellAt(r, 299, 149, 4, 2)).toEqual({ x: 3, y: 1 });
	expect(cellAt(r, 300, 100, 4, 2)).toBeNull();
	expect(cellAt(r, 99, 100, 4, 2)).toBeNull();
	expect(boxCells(r, [160, 60], [260, 120], 4, 2)).toHaveLength(6); // x 1..3, y 0..1
	expect(boxCells(r, [0, 0], [1000, 1000], 4, 2)).toHaveLength(8);
});

test('registry rows first, unknown workers get a stand-in', () => {
	const items = toItems({ n1: node() }, [wk({ id: 9, node_id: null, host: 'virt', addr: 'x' }) as never, wk({ id: 1, node_id: 'n1', cpu: 5 }) as never]);
	expect(items.map((i) => i.node.host)).toEqual(['pc-01', 'virt']);
	expect(items[0].w?.cpu).toBe(5);
});

test('tile labels fit a cell', () => {
	expect(tileLabel({ node_id: 'abc/v12', host: 'Divyanshs-MacBook-Air.local' })).toBe('v12');
	expect(tileLabel({ node_id: 'abc', host: 'lab-a-07.college.in' })).toBe('lab-a-07'.slice(0, 6) + '…');
	expect(tileLabel({ node_id: 'abc', host: 'pc12' })).toBe('pc12');
});

test('asleep and waking tiles draw an icon, not a glyph', () => {
	expect(look(item({ power: 'asleep', online: false }), 'state')).toMatchObject({ glyph: '', power: 'asleep', label: 'asleep', dim: false, dash: true });
	expect(look(item({ power: 'waking', online: false }), 'state', 23)).toMatchObject({ glyph: '', power: 'waking', stroke: 'stroke-warn' });
	expect(look(item({ power: 'waking', online: false }), 'state', 120)).toMatchObject({ power: 'failed', stroke: 'stroke-bad', label: 'did not wake' });
	expect(look(item(), 'state').power).toBeNull();
});
