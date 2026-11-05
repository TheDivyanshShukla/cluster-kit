// Pure floor-map helpers: what a tile looks like per overlay, auto-arrange order, grid hit-testing.
import type { NodeRow, NodeState, Worker } from './api/schema';
import { WAKE_FAIL_S } from './topology';

export const CELL = 44;
export type Overlay = 'state' | 'cpu' | 'work' | 'avail';
/** A registered PC plus its live connection numbers (null when offline or unknown to the cluster feed). */
export type Item = { node: NodeRow; w: Worker | null };
/** `power` is set while a PC is asleep or waking: the tile then draws a lucide icon (Moon / Sunrise) instead of a glyph. */
export type Look = { fill: string; stroke: string; text: string; glyph: string; label: string; dim: boolean; dash: boolean; power: 'asleep' | 'waking' | 'failed' | null };

// Tailwind needs whole class names in source, hence the literal tables. Every look has a glyph: never colour alone.
const tone = {
	ok: { fill: 'fill-ok/20', stroke: 'stroke-ok', text: 'fill-ok' },
	warn: { fill: 'fill-warn/20', stroke: 'stroke-warn', text: 'fill-warn' },
	bad: { fill: 'fill-bad/20', stroke: 'stroke-bad', text: 'fill-bad' },
	idle: { fill: 'fill-idle/15', stroke: 'stroke-idle', text: 'fill-idle' },
	busy: { fill: 'fill-busy/20', stroke: 'stroke-busy', text: 'fill-busy' }
} as const;

export const STATE_LOOK: Record<NodeState, { tone: keyof typeof tone; glyph: string }> = {
	pending: { tone: 'warn', glyph: '?' },
	active: { tone: 'ok', glyph: '✓' },
	cordoned: { tone: 'idle', glyph: '⊘' },
	draining: { tone: 'busy', glyph: '↓' },
	maintenance: { tone: 'bad', glyph: '≡' },
	retired: { tone: 'idle', glyph: '×' }
};
export const cpuTone = (cpu: number): keyof typeof tone => (cpu < 25 ? 'idle' : cpu < 60 ? 'ok' : cpu < 85 ? 'warn' : 'bad');

export type Avail = 'use' | 'blocked' | 'free' | 'off';
export const AVAIL: Record<Avail, { tone: keyof typeof tone; glyph: string; label: string }> = {
	use: { tone: 'busy', glyph: '●', label: 'in use by a person' },
	blocked: { tone: 'bad', glyph: '⊘', label: 'blocked by timetable' },
	free: { tone: 'ok', glyph: '✓', label: 'free for jobs' },
	off: { tone: 'idle', glyph: '–', label: 'not taking jobs' }
};
/** Why a PC can or cannot take work right now: a person at it, a timetable rule, or free. */
export function availability(node: NodeRow, w: Worker | null): Avail {
	if (node.state !== 'active' || !node.online) return 'off';
	if (w?.presence === 'busy' || node.presence === 'busy') return 'use';
	return w?.blocked ? 'blocked' : 'free';
}

/** `wakeSecs`: seconds since the PC was sent the magic packet (past WAKE_FAIL_S the tile says it did not wake). */
export function look({ node, w }: Item, overlay: Overlay, wakeSecs = 0): Look {
	let t: keyof typeof tone, glyph: string, label: string;
	if (overlay === 'cpu') {
		const cpu = w?.cpu;
		t = cpu == null ? 'idle' : cpuTone(cpu);
		glyph = cpu == null ? '–' : String(Math.round(cpu));
		label = cpu == null ? 'CPU unknown' : `CPU ${glyph}%`;
	} else if (overlay === 'work') {
		const busy = !!w?.busy;
		t = busy ? 'busy' : 'idle';
		glyph = busy ? '▶' : '–';
		label = busy ? 'working' : 'idle';
	} else if (overlay === 'avail') {
		const a = availability(node, w);
		t = AVAIL[a].tone;
		glyph = AVAIL[a].glyph;
		label = AVAIL[a].label;
	} else {
		t = STATE_LOOK[node.state].tone;
		glyph = STATE_LOOK[node.state].glyph;
		label = node.state;
	}
	const dim = !node.online && node.power === 'awake';
	const dash = overlay === 'state' && node.state === 'pending';
	// asleep: faint fill, thin dashed grey outline. waking: warn outline (bad once it gave up). No glyph: the icon says it.
	if (node.power === 'asleep') return { fill: 'fill-idle/35', stroke: 'stroke-line', text: 'fill-muted-foreground', glyph: '', label: 'asleep', dim: false, dash: true, power: 'asleep' };
	if (node.power === 'waking') {
		const failed = wakeSecs >= WAKE_FAIL_S;
		return { ...tone[failed ? 'bad' : 'warn'], glyph: '', label: failed ? 'did not wake' : 'waking', dim: false, dash: false, power: failed ? 'failed' : 'waking' };
	}
	return { ...tone[t], glyph, label, dim, dash, power: null };
}

const byHost = (a: Item, b: Item) =>
	a.node.host.localeCompare(b.node.host, undefined, { numeric: true }) || a.node.node_id.localeCompare(b.node.node_id);

/** Registered PCs in host-name order. Workers the registry does not know (virtual ones) get a stand-in row. */
export function toItems(nodes: Record<string, NodeRow>, workers: Worker[]): Item[] {
	const byId = new Map(workers.filter((w) => w.node_id).map((w) => [w.node_id as string, w]));
	const items: Item[] = Object.values(nodes).map((node) => ({ node, w: node.online ? (byId.get(node.node_id) ?? null) : null }));
	for (const w of workers) {
		if (w.node_id && nodes[w.node_id]) continue;
		items.push({
			w,
			node: {
				node_id: w.node_id ?? `w${w.id}`, host: w.host ?? w.addr, addr: w.addr, inventory: w.inv, first_seen_ms: 0, last_seen_ms: 0,
				state: 'active', state_reason: '', state_by: '', lab_id: null, x: null, y: null, online: true, labels: {}, pools: [], class: '', calib_mhs: null, presence: 'idle', power: 'awake'
			}
		});
	}
	return items.sort(byHost);
}

/** Fill the empty cells of a lab row by row with the given PCs in host order. Seats already taken are kept. */
export function autoArrange(pcs: Item[], w: number, h: number, taken: Iterable<string>): { id: string; x: number; y: number }[] {
	const used = new Set(taken); // "x,y"
	const out: { id: string; x: number; y: number }[] = [];
	const queue = [...pcs].sort(byHost);
	for (let y = 0; y < h && queue.length; y++)
		for (let x = 0; x < w && queue.length; x++)
			if (!used.has(`${x},${y}`)) out.push({ id: queue.shift()!.node.node_id, x, y });
	return out;
}

type Rect = { left: number; top: number; width: number; height: number };
/** The grid cell under a screen point; null outside the lab. Works at any scale of the SVG. */
export function cellAt(r: Rect, px: number, py: number, w: number, h: number): { x: number; y: number } | null {
	if (r.width <= 0 || r.height <= 0) return null;
	const fx = ((px - r.left) / r.width) * w, fy = ((py - r.top) / r.height) * h;
	if (fx < 0 || fy < 0 || fx >= w || fy >= h) return null;
	return { x: Math.floor(fx), y: Math.floor(fy) };
}

/** Every cell touched by a box dragged between two screen points (clamped to the lab). */
export function boxCells(r: Rect, a: [number, number], b: [number, number], w: number, h: number): { x: number; y: number }[] {
	if (r.width <= 0 || r.height <= 0) return [];
	const cx = (p: number) => ((p - r.left) / r.width) * w, cy = (p: number) => ((p - r.top) / r.height) * h;
	const clamp = (v: number, n: number) => Math.max(0, Math.min(n - 1, Math.floor(v)));
	const x0 = clamp(Math.min(cx(a[0]), cx(b[0])), w), x1 = clamp(Math.max(cx(a[0]), cx(b[0])), w);
	const y0 = clamp(Math.min(cy(a[1]), cy(b[1])), h), y1 = clamp(Math.max(cy(a[1]), cy(b[1])), h);
	const out: { x: number; y: number }[] = [];
	for (let y = y0; y <= y1; y++) for (let x = x0; x <= x1; x++) out.push({ x, y });
	return out;
}

/** Short tile text that fits a ~44 px cell: a virtual node's "/vN" suffix, else the host's first DNS label, max 7 chars. */
export function tileLabel(n: { node_id: string; host: string }): string {
	const v = /\/(v\d+)$/.exec(n.node_id);
	const base = v ? v[1] : (n.host || n.node_id).split('.')[0];
	return base.length > 7 ? base.slice(0, 6) + '…' : base;
}
