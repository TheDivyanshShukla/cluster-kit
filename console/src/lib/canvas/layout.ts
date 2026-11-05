// Force-free scene layout in world units. A PC's slot depends only on its lab, its seat or (if unplaced) a sticky cell:
// state changes never move it, and adding a PC never moves the others (docs/DESIGN.md section 7).
export type PcIn = { id: string; lab_id: number | null; x: number | null; y: number | null; threads: number };
export type LabIn = { id: number; name: string; w: number; h: number };
export type Slot = { id: string; x: number; y: number; r: number; lab: number };
export type LabBox = { id: number; name: string; x: number; y: number; w: number; h: number; count: number };
export type Scene = {
	labs: LabBox[];
	pcs: Slot[];
	index: Map<string, number>;
	/** lab id + cell per PC, pass back as `prev` for stickiness */
	cells: Map<string, string>;
	head: { x: number; y: number };
	standby: (i: number) => { x: number; y: number };
	w: number;
	h: number;
};

export const CELL = 20;
const PAD = 8;
const HEAD_H = 124;
const GAP = 14;
const TOP = 22; // lab title strip

/** 6-10 px circles by thread count (diameter in world units). */
export const radiusOf = (threads: number) => 3 + Math.min(Math.max(threads, 1), 32) / 32 * 2;

/** `narrow`: a portrait canvas (phone) packs labs into a ~280 unit column instead of a wide row. */
export function layoutScene(pcs: PcIn[], labs: LabIn[], prev: Map<string, string> = new Map(), narrow = false): Scene {
	const known = new Set(labs.map((l) => l.id));
	const groups = new Map<number, PcIn[]>();
	for (const p of pcs) {
		const g = p.lab_id != null && known.has(p.lab_id) ? p.lab_id : 0;
		(groups.get(g) ?? groups.set(g, []).get(g)!).push(p);
	}
	const defs: LabIn[] = [...labs].sort((a, b) => a.id - b.id);
	if (groups.has(0)) {
		const n = groups.get(0)!.length;
		defs.push({ id: 0, name: 'Unassigned', w: Math.max(4, Math.ceil(Math.sqrt(n * 1.6))), h: 1 });
	}
	// every lab gets a box; size grows only when more PCs need seats than the lab has
	const boxes: { def: LabIn; cols: number; rows: number; members: PcIn[] }[] = defs.map((def) => {
		const members = groups.get(def.id) ?? [];
		const cols = Math.max(1, def.w);
		const placedMax = members.reduce((m, p) => (p.x != null && p.y != null && p.x < cols ? Math.max(m, p.y * cols + p.x + 1) : m), 0);
		const need = Math.max(placedMax, members.length, 1);
		return { def, cols, rows: Math.max(def.h, Math.ceil(need / cols)), members };
	});
	const area = boxes.reduce((a, b) => a + (b.cols * CELL + PAD * 2) * (b.rows * CELL + PAD * 2 + TOP), 0);
	const rowW = narrow ? 280 : Math.max(520, Math.sqrt(area) * 1.5);

	const outLabs: LabBox[] = [];
	const out: Slot[] = [];
	const cells = new Map<string, string>();
	let x = 0, y = HEAD_H, rowH = 0, maxW = 0;
	for (const b of boxes) {
		const bw = b.cols * CELL + PAD * 2, bh = b.rows * CELL + PAD * 2 + TOP;
		if (x > 0 && x + bw > rowW) {
			x = 0;
			y += rowH + GAP;
			rowH = 0;
		}
		outLabs.push({ id: b.def.id, name: b.def.name, x, y, w: bw, h: bh, count: b.members.length });
		// seats: placed PCs first; then unplaced ones keep a previous free cell, the rest take the lowest free cells in id order
		const taken = new Set<number>();
		const seat = new Map<string, number>();
		for (const p of b.members) {
			if (p.x != null && p.y != null && p.x < b.cols && p.y < b.rows && !taken.has(p.y * b.cols + p.x)) {
				seat.set(p.id, p.y * b.cols + p.x);
				taken.add(p.y * b.cols + p.x);
			}
		}
		const rest = b.members.filter((p) => !seat.has(p.id)).sort((a, c) => (a.id < c.id ? -1 : a.id > c.id ? 1 : 0));
		for (const p of rest) {
			const m = /^(\d+):(\d+)$/.exec(prev.get(p.id) ?? '');
			if (m && Number(m[1]) === b.def.id && Number(m[2]) < b.cols * b.rows && !taken.has(Number(m[2]))) {
				seat.set(p.id, Number(m[2]));
				taken.add(Number(m[2]));
			}
		}
		let free = 0;
		for (const p of rest) {
			if (seat.has(p.id)) continue;
			while (taken.has(free)) free++;
			seat.set(p.id, free);
			taken.add(free);
		}
		for (const p of b.members) {
			const c = seat.get(p.id)!;
			cells.set(p.id, `${b.def.id}:${c}`);
			out.push({ id: p.id, lab: b.def.id, r: radiusOf(p.threads), x: x + PAD + (c % b.cols) * CELL + CELL / 2, y: y + PAD + TOP + Math.floor(c / b.cols) * CELL + CELL / 2 });
		}
		x += bw + GAP;
		rowH = Math.max(rowH, bh);
		maxW = Math.max(maxW, x - GAP);
	}
	const w = Math.max(maxW, 320), h = y + rowH;
	const head = { x: w / 2, y: 30 };
	return { labs: outLabs, pcs: out, index: new Map(out.map((s, i) => [s.id, i])), cells, head, standby: (i) => ({ x: head.x + 84 + i * 64, y: head.y }), w: Math.max(w, head.x + 84 + 2 * 64 + 24), h };
}

export type View = { k: number; tx: number; ty: number };
/** Scale and offset that fit the whole scene in a w x h box with a margin. */
export function fitView(scene: Pick<Scene, 'w' | 'h'>, w: number, h: number, margin = 12, maxK = 3): View {
	const k = Math.max(0.05, Math.min(maxK, (w - margin * 2) / scene.w, (h - margin * 2) / scene.h));
	return { k, tx: (w - scene.w * k) / 2, ty: Math.max(margin, (h - scene.h * k) / 2) };
}
export const toWorld = (v: View, sx: number, sy: number) => ({ x: (sx - v.tx) / v.k, y: (sy - v.ty) / v.k });

/** The PC under a world point (within its radius plus slack), nearest wins. */
export function hitPc(scene: Scene, wx: number, wy: number, slack = 3): Slot | null {
	let best: Slot | null = null, bd = Infinity;
	for (const s of scene.pcs) {
		const d = Math.hypot(s.x - wx, s.y - wy);
		if (Math.max(Math.abs(s.x - wx), Math.abs(s.y - wy)) <= s.r + 2 + slack && d < bd) {
			best = s;
			bd = d;
		}
	}
	return best;
}

/** Scale and offset that fit a world box in a w x h screen box, leaving `top` and `bottom` px free. */
export function fitBox(b: { x: number; y: number; w: number; h: number }, w: number, h: number, top = 0, bottom = 0, margin = 16, maxK = 10): View {
	const aw = Math.max(40, w - margin * 2), ah = Math.max(40, h - top - bottom - margin * 2);
	const k = Math.max(0.05, Math.min(maxK, aw / Math.max(1, b.w), ah / Math.max(1, b.h)));
	return { k, tx: (w - b.w * k) / 2 - b.x * k, ty: top + margin + (ah - b.h * k) / 2 - b.y * k };
}
