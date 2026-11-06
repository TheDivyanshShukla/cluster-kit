// The canvas renderer: owns the layout, view, particles and effects, and draws only when something changed or moves.
// The Svelte component feeds it data (update / setEvent) and pointer input; nothing here touches the DOM except the canvas.
import type { EventRow, HaStatus, JobRow, LabRow, NodeRow, Worker } from '../api/schema';
import { toItems } from '../map';
import { effectFor, hostIndex, schedule, type HostIndex, type Live, type Tone } from './effects';
import { jobColor, readTokens, type Tokens } from './colors';
import { heat, layerValue, lodOf, type Layer, type Lod } from './layers';
import { rate as fmtRate } from '../format';
import { drawGlyph } from './glyphs';
import { fitBox, fitView, hitPc, layoutScene, toWorld, type PcIn, type Scene, type View } from './layout';
import { Particles, planParticles } from './particles';
import type { NodeSnap } from './replay';
import { attrsOf, classOf, CLASSES, jobSlots, makeJobFinder, type Cls } from './state';
import type { Attrs, Matcher } from './select';
import type { CanvasHead } from '../hier';

export const HUGE = 2000;
export type Input = {
	nodes: Record<string, NodeRow>;
	labs: LabRow[];
	workers: Worker[];
	jobs: JobRow[];
	ha: Pick<HaStatus, 'role' | 'epoch' | 'standbys'> | null;
	filter: Matcher | null;
	/** replay: node states at an earlier time; the workers are then ignored */
	overrides: Record<string, NodeSnap> | null;
	/** node id of the head once a promotion told us (else the head is just "HEAD") */
	headId: string | null;
	/** node_id -> losses in 24 h (Reliability layer) and head score 0-100 (Head fitness layer) */
	losses?: Record<string, number>;
	scores?: Record<string, number>;
	/** lab heads (docs/HIERARCHY.md), drawn on their lab box */
	hier?: CanvasHead[];
};
type Group = { cls: Cls; fill: string; a: number; dim: boolean; idx: number[] };
export type LabStat = { busy: number; awake: number; total: number; rate: number; share: number };
const TAU = Math.PI * 2;
const ease = (u: number) => 1 - (1 - u) ** 3;

export class LiveEngine {
	ctx: CanvasRenderingContext2D;
	tokens!: Tokens;
	scene: Scene = layoutScene([], []);
	view: View = { k: 1, tx: 0, ty: 0 };
	userView = false;
	/** screen px kept free at the bottom for the legend and at the top for the toolbar */
	padBottom = 0;
	padTop = 0;
	layer: Layer = 'status';
	selected: string | null = null;
	/** keyboard focus ring (set only while the canvas has focus) */
	focused: string | null = null;
	follow: number | null = null;
	bundles = false;
	lod: Lod = 'square';
	labStats = new Map<number, LabStat>();
	/** lab heads by lab name */
	labHeads = new Map<string, CanvasHead>();
	maxRate = 0;
	maxLoss = 0;
	/** called after the user (or fit/follow) changed the view */
	onview: (() => void) | null = null;
	cpu = new Float32Array(0);
	ram = new Float32Array(0);
	rateOf = new Float32Array(0);
	loss = new Uint16Array(0);
	score = new Float32Array(0);
	mini: { x: number; y: number; w: number; h: number; s: number } | null = null;
	private jobNames = new Map<number, string>();
	private hostOfI: string[] = [];
	w = 300;
	h = 200;
	dpr = 1;
	reduce = false;
	huge = false;
	replay = false;
	dirty = true;
	drawMs: number[] = [];
	items: ReturnType<typeof toItems> = [];
	cls: Cls[] = [];
	jobOf: (number | null)[] = [];
	/** PCs showing their identify banner: index -> until (epoch ms) */
	private idents = new Map<string, number>();
	counts: Record<Cls, number> = { busy: 0, idle: 0, use: 0, cord: 0, asleep: 0, waking: 0, lost: 0 };
	jobIds = new Map<number, number>();
	hosts: HostIndex = new Map();
	private groups: Group[] = [];
	private sig = '';
	private dim: Uint8Array = new Uint8Array(0);
	private particles = new Particles();
	private fx: Live[] = [];
	private flashes = new Map<string, number>();
	private prevChunks = new Map<string, number>();
	private seenEvents = new Set<number>();
	private since = 0;
	private standbyIds = new Set<string>();
	private input: Input | null = null;
	// head / failover state
	private role: HaStatus['role'] = 'primary';
	private epoch: number | null = null;
	private shown: { id: string; lag: number }[] = [];
	private born = new Map<string, number>();
	private known = new Set<string>();
	private lost = false;
	private glide: { id: string; t0: number } | null = null;
	private roll: { from: number; t0: number } | null = null;
	private ghost = 0;
	headId: string | null = null;

	constructor(public canvas: HTMLCanvasElement, themeHost: HTMLElement) {
		this.ctx = canvas.getContext('2d')!;
		this.since = performance.now();
		this.reduce = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
		this.retheme(themeHost);
	}
	retheme(host: HTMLElement) {
		this.tokens = readTokens(host);
		if (this.cls.length) this.buildGroups();
		this.dirty = true;
	}
	resize(w: number, h: number, dpr: number) {
		this.w = Math.max(1, w);
		this.h = Math.max(1, h);
		this.dpr = dpr;
		this.canvas.width = Math.round(this.w * dpr);
		this.canvas.height = Math.round(this.h * dpr);
		if (!this.userView) this.view = this.fit();
		this.dirty = true;
	}
	resetView() {
		this.userView = false;
		this.follow = null;
		this.view = this.fit();
		this.dirty = true;
		this.onview?.();
	}
	private fit() {
		const v = fitView(this.scene, this.w, Math.max(60, this.h - this.padBottom - this.padTop));
		return { ...v, ty: v.ty + this.padTop };
	}
	setPad(bottom: number, top = this.padTop) {
		if (bottom === this.padBottom && top === this.padTop) return;
		this.padBottom = bottom;
		this.padTop = top;
		if (!this.userView) this.view = this.fit();
		this.dirty = true;
	}
	zoomAt(sx: number, sy: number, f: number) {
		const k = Math.min(10, Math.max(0.1, this.view.k * f));
		const wp = toWorld(this.view, sx, sy);
		this.view = { k, tx: sx - wp.x * k, ty: sy - wp.y * k };
		this.touch();
	}
	panBy(dx: number, dy: number) {
		this.view = { ...this.view, tx: this.view.tx + dx, ty: this.view.ty + dy };
		this.touch();
	}
	private touch() {
		this.userView = true;
		this.follow = null;
		this.dirty = true;
		this.onview?.();
	}
	/** world point at the middle of the free canvas area */
	centre() {
		return toWorld(this.view, this.w / 2, this.padTop + (this.h - this.padTop - this.padBottom) / 2);
	}
	setView(k: number, cx: number, cy: number) {
		this.view = { k, tx: this.w / 2 - cx * k, ty: this.padTop + (this.h - this.padTop - this.padBottom) / 2 - cy * k };
		this.userView = true;
		this.dirty = true;
	}
	centerOn(wx: number, wy: number) {
		this.setView(this.view.k, wx, wy);
		this.touch();
	}
	fitLab(id: number) {
		const l = this.scene.labs.find((x) => x.id === id);
		if (!l) return;
		this.view = fitBox(l, this.w, this.h, this.padTop, this.padBottom);
		this.touch();
	}
	setFollow(job: number | null) {
		this.follow = job;
		if (job != null) this.followFit();
		else this.dirty = true;
	}
	private followFit() {
		let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
		for (let i = 0; i < this.jobOf.length; i++) {
			if (this.jobOf[i] !== this.follow) continue;
			const s = this.scene.pcs[i];
			x0 = Math.min(x0, s.x - 12); y0 = Math.min(y0, s.y - 12); x1 = Math.max(x1, s.x + 12); y1 = Math.max(y1, s.y + 12);
		}
		if (x1 < x0) return;
		this.view = fitBox({ x: x0, y: y0, w: x1 - x0, h: y1 - y0 }, this.w, this.h, this.padTop, this.padBottom, 16, 4);
		this.userView = true;
		this.dirty = true;
		this.onview?.();
	}
	setLayer(l: Layer) {
		if (l === this.layer) return;
		this.layer = l;
		this.buildGroups();
		this.dirty = true;
	}
	setSelected(id: string | null) {
		this.selected = id;
		this.dirty = true;
	}
	setFocused(id: string | null) {
		this.focused = id;
		this.dirty = true;
	}
	/** minimap click: the world point under it, or null when the click is elsewhere */
	miniAt(sx: number, sy: number): { x: number; y: number } | null {
		const m = this.mini;
		if (!m || sx < m.x || sx > m.x + m.w || sy < m.y || sy > m.y + m.h) return null;
		return { x: (sx - m.x) / m.s, y: (sy - m.y) / m.s };
	}
	/** per-PC numbers for the tooltip and the side panel */
	metricsOf(id: string) {
		const i = this.scene.index.get(id);
		if (i == null) return null;
		const f = (v: number) => (Number.isNaN(v) ? null : v);
		return { cls: this.cls[i], job: this.jobOf[i], cpu: f(this.cpu[i]), ram: f(this.ram[i]), rate: f(this.rateOf[i]), losses: this.loss[i], score: f(this.score[i]) };
	}
	/** PC id under a screen point. */
	hit(sx: number, sy: number): string | null {
		const p = toWorld(this.view, sx, sy);
		return hitPc(this.scene, p.x, p.y, 3 / this.view.k + 1)?.id ?? null;
	}
	/** Screen position of a PC (for tooltips). */
	screenOf(id: string): { x: number; y: number } | null {
		const i = this.scene.index.get(id);
		if (i == null) return null;
		const s = this.scene.pcs[i];
		return { x: s.x * this.view.k + this.view.tx, y: s.y * this.view.k + this.view.ty };
	}
	flash(ids: string[], now = performance.now()) {
		for (const id of ids) this.flashes.set(id, now + 2200);
		this.dirty = true;
	}

	// ------------------------------------------------------------ data in
	update(inp: Input, now = performance.now()) {
		this.input = inp;
		this.labHeads = new Map((inp.hier ?? []).map((h) => [h.lab, h]));
		this.replay = inp.overrides != null;
		const items = toItems(inp.nodes, this.replay ? [] : inp.workers).filter((i) => i.node.state !== 'retired');
		this.items = items;
		this.hosts = hostIndex(items.map((i) => i.node));
		const pcs: PcIn[] = items.map(({ node, w }) => ({ id: node.node_id, lab_id: node.lab_id, x: node.x, y: node.y, threads: node.inventory?.cores_logical ?? w?.threads ?? 4 }));
		const narrow = this.w < this.h * 0.9;
		const sig = `${narrow}|${inp.labs.map((l) => `${l.id}:${l.w}x${l.h}`).join(',')}|${pcs.map((p) => `${p.id}.${p.lab_id}.${p.x}.${p.y}.${p.threads}`).join(',')}`;
		if (sig !== this.sig) {
			this.sig = sig;
			this.scene = layoutScene(pcs, inp.labs, this.scene.cells, narrow);
			if (!this.userView) this.view = this.fit();
		}
		this.huge = items.length > HUGE;
		this.jobIds = jobSlots(inp.jobs);
		const finder = makeJobFinder(inp.jobs);
		const labName = new Map(inp.labs.map((l) => [l.id, l.name]));
		const n = this.scene.pcs.length;
		this.cls = new Array(n);
		this.jobOf = new Array(n).fill(null);
		const attrs: Attrs[] = new Array(n);
		this.counts = { busy: 0, idle: 0, use: 0, cord: 0, asleep: 0, waking: 0, lost: 0 };
		this.cpu = new Float32Array(n).fill(NaN);
		this.ram = new Float32Array(n).fill(NaN);
		this.rateOf = new Float32Array(n).fill(NaN);
		this.loss = new Uint16Array(n);
		this.score = new Float32Array(n).fill(NaN);
		this.hostOfI = new Array(n);
		this.jobNames = new Map(inp.jobs.map((j) => [j.id, j.name]));
		this.maxRate = 0;
		this.maxLoss = 0;
		const labs = new Map<number, LabStat>();
		for (const { node, w } of items) {
			const i = this.scene.index.get(node.node_id)!;
			const o = inp.overrides?.[node.node_id];
			const view = o ? { ...node, ...o } : node;
			const c = classOf(view, w);
			if (node.identify_until_ms) this.idents.set(node.node_id, node.identify_until_ms);
			else this.idents.delete(node.node_id);
			this.cls[i] = c;
			this.counts[c]++;
			const a = attrsOf(node, labName.get(node.lab_id ?? -1), c, null, this.scene.pcs[i].r > 4 ? 16 : 8);
			if (c === 'busy') this.jobOf[i] = finder(a);
			a.job = this.jobOf[i] == null ? undefined : String(this.jobOf[i]);
			attrs[i] = a;
			this.hostOfI[i] = node.host;
			if (w && !this.replay) {
				this.cpu[i] = w.cpu;
				this.ram[i] = w.mem_total > 0 ? (w.mem_used / w.mem_total) * 100 : NaN;
				this.rateOf[i] = w.rate;
				this.maxRate = Math.max(this.maxRate, w.rate);
			}
			this.loss[i] = inp.losses?.[node.node_id] ?? 0;
			this.maxLoss = Math.max(this.maxLoss, this.loss[i]);
			const sc = inp.scores?.[node.node_id];
			if (sc != null) this.score[i] = sc;
			const ls = labs.get(this.scene.pcs[i].lab) ?? labs.set(this.scene.pcs[i].lab, { busy: 0, awake: 0, total: 0, rate: 0, share: 0 }).get(this.scene.pcs[i].lab)!;
			ls.total++;
			if (c === 'busy') ls.busy++;
			if (c !== 'asleep' && c !== 'lost' && c !== 'waking') ls.awake++;
			if (!Number.isNaN(this.rateOf[i])) ls.rate += this.rateOf[i];
		}
		const sum = [...labs.values()].reduce((a, l) => a + l.rate, 0);
		for (const l of labs.values()) l.share = sum > 0 ? l.rate / sum : 0;
		this.labStats = labs;
		this.dim = new Uint8Array(n);
		if (inp.filter) for (let i = 0; i < n; i++) this.dim[i] = inp.filter(attrs[i]) ? 0 : 1;
		this.buildGroups();
		if (this.follow != null && !this.replay) this.followFit();
		this.setHa(inp, now);
		if (!this.replay) this.ingestChunks(inp.workers, now);
		else this.particles.clear();
		this.dirty = true;
	}
	/** how many PCs match the filter (all when none) */
	get matched() {
		return this.dim.length - this.dim.reduce((a, d) => a + d, 0);
	}
	/** fill colour and alpha of PC i for the active layer */
	private paint(i: number): { fill: string; a: number } {
		const T = this.tokens, c = this.cls[i];
		if (c === 'lost') return { fill: T.bad, a: 1 };
		if (c === 'asleep') return { fill: T.idle, a: 0.35 };
		if (c === 'waking') return { fill: T.warn, a: 0.5 };
		switch (this.layer) {
			case 'status': return c === 'busy' ? { fill: T.primary, a: 1 } : { fill: T.surface, a: 1 };
			case 'power': return { fill: T.ok, a: 0.85 };
			case 'job': {
				const j = this.jobOf[i];
				return c === 'busy' ? { fill: jobColor(T, j == null ? -1 : (this.jobIds.get(j) ?? -1)), a: 1 } : { fill: T.surface, a: 1 };
			}
			default: {
				const f = (v: number) => (Number.isNaN(v) ? null : v);
				const v = layerValue(this.layer, { cpu: f(this.cpu[i]), rate: f(this.rateOf[i]), losses: this.loss[i], score: f(this.score[i]) }, { rate: this.maxRate, losses: this.maxLoss });
				return v == null ? { fill: T.idle, a: 0.25 } : { fill: heat(v), a: 1 };
			}
		}
	}
	private buildGroups() {
		const m = new Map<string, Group>();
		for (let i = 0; i < this.cls.length; i++) {
			const cls = this.cls[i];
			const dim = this.dim[i] === 1;
			const { fill, a } = this.paint(i);
			const key = `${cls}|${fill}|${a}|${dim}`;
			(m.get(key) ?? m.set(key, { cls, fill, a, dim, idx: [] }).get(key)!).idx.push(i);
		}
		this.groups = [...m.values()];
	}

	private ingestChunks(workers: Worker[], now: number) {
		const seen = new Map<string, number>();
		const deltas: { id: string; n: number; job: number | null }[] = [];
		for (const w of workers) {
			const id = w.node_id ?? `w${w.id}`;
			seen.set(id, w.chunks);
			const p = this.prevChunks.get(id);
			if (p != null && w.chunks > p) {
				const i = this.scene.index.get(id);
				deltas.push({ id, n: w.chunks - p, job: i == null ? null : (this.jobOf[i] == null ? null : (this.jobIds.get(this.jobOf[i]!) ?? -1)) });
			}
		}
		this.prevChunks = seen;
		if (this.reduce || this.huge || document.hidden) return;
		const head = this.headPos(now);
		for (const p of planParticles(deltas, this.particles.room)) {
			const i = this.scene.index.get(p.id);
			if (i == null) continue;
			const s = this.scene.pcs[i];
			if (p.dir === 'out') this.particles.spawn(head.x, head.y, s.x, s.y, now + p.delay, 450, p.job);
			else this.particles.spawn(s.x, s.y, head.x, head.y, now + p.delay, 450, p.job);
		}
	}

	/** An event from the stream or the replay: ripples and glyphs at its PCs. Events older than when the canvas opened do nothing. */
	setEvent(ev: EventRow, now = performance.now()) {
		if (this.seenEvents.has(ev.id)) return;
		this.seenEvents.add(ev.id);
		if (this.replay || this.reduce || Date.now() - ev.ts_ms > 10_000) return;
		const fx = effectFor(ev, this.hosts);
		if (!fx) return;
		if (fx.type === 'spec') {
			const head = this.headPos(now);
			for (const id of fx.ids) {
				const i = this.scene.index.get(id);
				if (i != null) this.particles.spawn(head.x, head.y, this.scene.pcs[i].x, this.scene.pcs[i].y, now, 700, null, true);
			}
		}
		this.fx = schedule(this.fx, fx, now);
		this.dirty = true;
	}

	private setHa(inp: Input, now: number) {
		const h = inp.ha;
		if (inp.headId !== this.headId) this.headId = inp.headId;
		if (!h) return;
		const first = this.epoch == null;
		const cur = h.standbys.map((s) => ({ id: s.node_id, lag: s.lag }));
		if (h.role === 'candidate') {
			if (!this.lost) this.lost = true;
		} else {
			this.lost = false;
			if (!first && h.epoch > this.epoch!) {
				const p = this.shown[0];
				if (p && !this.reduce) this.glide = { id: p.id, t0: now };
				if (!this.reduce) {
					this.roll = { from: this.epoch!, t0: now };
					this.ghost = now;
				}
			}
			for (const s of cur) if (!first && !this.known.has(s.id) && !this.reduce) this.born.set(s.id, now);
			this.known = new Set(cur.map((s) => s.id));
			this.shown = cur;
		}
		this.role = h.role;
		this.epoch = h.epoch;
		this.standbyIds = new Set(this.shown.map((s) => s.id));
	}
	private headPos(_now: number) {
		return this.scene.head;
	}

	// ------------------------------------------------------------ drawing
	get animating() {
		const now = performance.now();
		return this.particles.n > 0 || this.fx.some((f) => now - f.t0 < f.dur) || this.flashes.size > 0 || !!this.glide || !!this.roll || now - this.ghost < 700 || this.born.size > 0 || (!this.reduce && this.counts.waking > 0) || this.identifying();
	}
	private identifying() {
		const t = Date.now();
		for (const u of this.idents.values()) if (u > t) return true;
		return false;
	}
	frame(now: number) {
		this.particles.prune(now);
		if (this.glide && now - this.glide.t0 > 640) this.glide = null;
		if (this.roll && now - this.roll.t0 > 400) this.roll = null;
		for (const [k, t] of this.flashes) if (now > t) this.flashes.delete(k);
		for (const [k, t] of this.born) if (now - t > 700) this.born.delete(k);
		if (!this.dirty && !this.animating) return;
		const t0 = performance.now();
		this.draw(now);
		this.dirty = false;
		this.drawMs.push(performance.now() - t0);
		if (this.drawMs.length > 600) this.drawMs.shift();
	}

	private draw(now: number) {
		const { ctx, tokens: T, view: v, dpr } = this;
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.clearRect(0, 0, this.w, this.h);
		ctx.setTransform(dpr * v.k, 0, 0, dpr * v.k, dpr * v.tx, dpr * v.ty);
		this.lod = this.huge ? 'point' : lodOf(18 * v.k);
		// lab regions with a header strip
		ctx.lineWidth = 1 / v.k;
		for (const l of this.scene.labs) {
			ctx.beginPath();
			ctx.roundRect(l.x, l.y, l.w, l.h, 8);
			ctx.globalAlpha = 0.7;
			ctx.fillStyle = T.surface;
			ctx.fill();
			ctx.globalAlpha = 1;
			ctx.strokeStyle = T.line;
			ctx.stroke();
			ctx.beginPath();
			ctx.roundRect(l.x, l.y, l.w, 22, [8, 8, 0, 0]);
			ctx.fillStyle = T.raised;
			ctx.fill();
		}
		if (this.bundles) this.drawBundles();
		this.drawHeadRow(now);
		this.drawPcs(now);
		this.drawParticles(now);
		this.drawFx(now);
		// screen-space text on top
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		if (this.lod === 'card') this.drawCards();
		this.drawLabels(now);
		this.drawMini();
	}

	/** one curved band per lab from the head, width = the lab's share of the work */
	private drawBundles() {
		const { ctx, tokens: T, scene: S } = this;
		const top = Math.max(1, ...[...this.labStats.values()].map((l) => l.rate));
		ctx.strokeStyle = T.primary;
		ctx.lineCap = 'round';
		for (const l of S.labs) {
			const st = this.labStats.get(l.id);
			if (!st || st.rate <= 0) continue;
			const x = l.x + l.w / 2, y = l.y;
			ctx.lineWidth = 1.5 + (st.rate / top) * 10;
			ctx.globalAlpha = 0.3;
			ctx.beginPath();
			ctx.moveTo(S.head.x, S.head.y + 16);
			ctx.quadraticCurveTo(S.head.x, (S.head.y + y) / 2 + 20, x, y);
			ctx.stroke();
		}
		ctx.globalAlpha = 1;
		ctx.lineCap = 'butt';
	}

	private drawPcs(now: number) {
		const { ctx, tokens: T, view: v } = this;
		const pulse = this.reduce ? 0.6 : 0.5 + 0.4 * Math.sin(now / 280);
		const k = v.k;
		const x0 = -v.tx / k - 12, y0 = -v.ty / k - 12, x1 = (this.w - v.tx) / k + 12, y1 = (this.h - v.ty) / k + 12;
		const pcs = this.scene.pcs;
		const vis = (s: { x: number; y: number }) => s.x > x0 && s.x < x1 && s.y > y0 && s.y < y1;
		const point = this.lod === 'point';
		const card = this.lod === 'card';
		const half = (i: number) => (card ? 9 : point ? Math.max(pcs[i].r + 2, 1.4 / k) : pcs[i].r + 2);
		const hair = 1.1 / Math.max(1, k * 0.6);
		for (const g of this.groups) {
			ctx.beginPath();
			let any = false;
			for (const i of g.idx) {
				const s = pcs[i];
				if (!vis(s)) continue;
				any = true;
				const h = half(i);
				if (point) ctx.rect(s.x - h, s.y - h, h * 2, h * 2);
				else ctx.roundRect(s.x - h, s.y - h, h * 2, h * 2, Math.min(3, h * 0.35));
			}
			if (!any) continue;
			const a = g.dim ? 0.15 : 1;
			ctx.globalAlpha = a * (card ? 1 : g.a) * (g.cls === 'waking' ? pulse + 0.4 : 1);
			ctx.fillStyle = card ? T.surface : g.fill;
			ctx.fill();
			if (point) continue;
			ctx.globalAlpha = a;
			ctx.lineWidth = (g.cls === 'use' ? 1.6 : hair);
			ctx.setLineDash(g.cls === 'asleep' ? [2, 2] : []);
			const stroke = g.cls === 'use' || g.cls === 'waking' ? T.warn : g.cls === 'lost' ? T.bad : g.fill === T.surface || g.cls === 'asleep' || g.cls === 'cord' ? T.idle : card ? T.line : null;
			if (stroke) {
				ctx.strokeStyle = stroke;
				ctx.stroke();
			}
			ctx.setLineDash([]);
			if (card) {
				// the layer's value rides on a band across the top of the card
				ctx.globalAlpha = a * g.a;
				ctx.fillStyle = g.fill;
				ctx.beginPath();
				for (const i of g.idx) {
					const s = pcs[i];
					if (vis(s)) ctx.roundRect(s.x - 9, s.y - 9, 18, 3.2, [3, 3, 0, 0]);
				}
				ctx.fill();
			}
		}
		ctx.globalAlpha = 1;
		if (!point) {
			// cordoned or draining: a diagonal through the square; badges in the corner (shape, never colour alone)
			const diag = new Path2D(), dotUse = new Path2D(), crescent = new Path2D(), ringWake = new Path2D(), cross = new Path2D();
			const stripes = new Map<number, Path2D>();
			const ring = new Path2D();
			let nDiag = 0;
			for (let i = 0; i < pcs.length; i++) {
				const s = pcs[i];
				if (!vis(s) || this.dim[i] === 1) continue;
				const h = half(i), c = this.cls[i];
				if (c === 'cord') (diag.moveTo(s.x - h * 0.7, s.y + h * 0.7), diag.lineTo(s.x + h * 0.7, s.y - h * 0.7), nDiag++);
				const bx = s.x + h - 1, by = s.y - h + 1;
				if (c === 'use') (dotUse.moveTo(bx + 2.2, by), dotUse.arc(bx, by, 2.2, 0, TAU));
				else if (c === 'asleep') (crescent.moveTo(bx + Math.cos(Math.PI * 0.6) * 2.2, by + Math.sin(Math.PI * 0.6) * 2.2), crescent.arc(bx, by, 2.2, Math.PI * 0.6, Math.PI * 1.9));
				else if (c === 'waking') (ringWake.moveTo(bx + 2.2, by), ringWake.arc(bx, by, 2.2, 0, TAU));
				else if (c === 'lost') (cross.moveTo(bx - 2, by - 2), cross.lineTo(bx + 2, by + 2), cross.moveTo(bx + 2, by - 2), cross.lineTo(bx - 2, by + 2));
				if (!card && k * h * 2 >= 9) {
					const cpu = this.cpu[i];
					if (!Number.isNaN(cpu) && c !== 'asleep' && c !== 'lost' && cpu > 0) (ring.moveTo(s.x, s.y - h - 1.6), ring.arc(s.x, s.y, h + 1.6, -Math.PI / 2, -Math.PI / 2 + Math.min(1, cpu / 100) * TAU - 0.001));
					const j = c === 'busy' ? this.jobOf[i] : null;
					if (j != null) {
						const slot = this.jobIds.get(j) ?? -1;
						(stripes.get(slot) ?? stripes.set(slot, new Path2D()).get(slot)!).rect(s.x - h, s.y - h + 2, Math.max(2.4, 2 / k), h * 2 - 4);
					}
				}
			}
			ctx.lineWidth = 1.2 / Math.max(1, k * 0.6);
			ctx.strokeStyle = T.idle;
			if (nDiag) ctx.stroke(diag);
			ctx.strokeStyle = T.warn; ctx.fillStyle = T.warn;
			ctx.fill(dotUse);
			ctx.stroke(ringWake);
			ctx.strokeStyle = T.idle;
			ctx.stroke(crescent);
			ctx.strokeStyle = T.bad;
			ctx.stroke(cross);
			ctx.lineWidth = 1.5 / Math.max(1, k * 0.7);
			ctx.strokeStyle = T.fg;
			ctx.globalAlpha = 0.65;
			ctx.stroke(ring);
			ctx.globalAlpha = 1;
			for (const [slot, p] of stripes) {
				ctx.fillStyle = jobColor(T, slot);
				ctx.fill(p);
			}
		}
		// standbys wear a dashed ring; flashed PCs a pulsing primary ring; the selected PC a halo
		if (!this.huge && !point) {
			ctx.lineWidth = 1 / Math.max(1, k * 0.6);
			ctx.strokeStyle = T.muted;
			ctx.setLineDash([1.5, 1.5]);
			ctx.beginPath();
			for (const id of this.standbyIds) {
				const i = this.scene.index.get(id);
				if (i == null) continue;
				const s = pcs[i];
				ctx.moveTo(s.x + s.r + 4.5, s.y);
				ctx.arc(s.x, s.y, s.r + 4.5, 0, TAU);
			}
			ctx.stroke();
			ctx.setLineDash([]);
		}
		// identify: a wide primary ring that keeps pulsing until the head says the banner is off
		const wall = Date.now();
		for (const [id, until] of this.idents) {
			if (until <= wall) continue;
			const i = this.scene.index.get(id);
			if (i == null) continue;
			const s = pcs[i];
			const ph = this.reduce ? 0.5 : (now % 1100) / 1100;
			ctx.globalAlpha = this.reduce ? 0.9 : 1 - ph;
			ctx.strokeStyle = T.primary;
			ctx.lineWidth = 2.4 / Math.max(1, k * 0.6);
			ctx.beginPath();
			ctx.arc(s.x, s.y, s.r + 5 + ph * 9, 0, TAU);
			ctx.stroke();
		}
		ctx.globalAlpha = 1;
		for (const [id, until] of this.flashes) {
			const i = this.scene.index.get(id);
			if (i == null) continue;
			const s = pcs[i];
			const u = 1 - (until - now) / 2200;
			ctx.globalAlpha = Math.max(0, 1 - u) * (0.6 + 0.4 * Math.sin(now / 90));
			ctx.strokeStyle = T.primary;
			ctx.lineWidth = 2 / Math.max(1, k * 0.6);
			ctx.beginPath();
			ctx.arc(s.x, s.y, s.r + 6 + u * 6, 0, TAU);
			ctx.stroke();
		}
		ctx.globalAlpha = 1;
		const si = this.selected ? this.scene.index.get(this.selected) : null;
		if (si != null) {
			const s = pcs[si];
			const r = half(si) + 4;
			ctx.strokeStyle = T.primary;
			ctx.lineWidth = 2.4 / Math.max(1, k * 0.7);
			ctx.globalAlpha = 0.25;
			ctx.beginPath();
			ctx.roundRect(s.x - r - 2, s.y - r - 2, (r + 2) * 2, (r + 2) * 2, 6);
			ctx.stroke();
			ctx.globalAlpha = 1;
			ctx.beginPath();
			ctx.roundRect(s.x - r, s.y - r, r * 2, r * 2, 5);
			ctx.stroke();
		}
		const fi = this.focused ? this.scene.index.get(this.focused) : null;
		if (fi != null) {
			const s = pcs[fi];
			const r = half(fi) + 7;
			const w = 3 / Math.max(1, k * 0.7);
			ctx.lineWidth = w + 3 / Math.max(1, k * 0.7);
			ctx.strokeStyle = T.surface;
			ctx.beginPath();
			ctx.roundRect(s.x - r, s.y - r, r * 2, r * 2, 6);
			ctx.stroke();
			ctx.lineWidth = w;
			ctx.strokeStyle = T.fg;
			ctx.stroke();
		}
	}

	/** near zoom: a mini card per visible PC, drawn in screen space */
	private drawCards() {
		const { ctx, tokens: T, view: v } = this;
		ctx.textBaseline = 'middle';
		ctx.textAlign = 'left';
		const pcs = this.scene.pcs;
		for (let i = 0; i < pcs.length; i++) {
			const s = pcs[i];
			const sx = s.x * v.k + v.tx, sy = s.y * v.k + v.ty;
			const half = 9 * v.k;
			if (sx + half < 0 || sy + half < 0 || sx - half > this.w || sy - half > this.h) continue;
			const px = half * 2, x = sx - half, y = sy - half, pad = Math.max(3, px * 0.06), band = 3.2 * v.k;
			const dim = this.dim[i] === 1;
			const c = this.cls[i];
			ctx.globalAlpha = dim ? 0.15 : 1;
			const f = Math.min(13, Math.max(9, px / 12));
			ctx.font = `600 ${f}px "Inter Variable", ui-sans-serif, system-ui, sans-serif`;
			ctx.fillStyle = T.fg;
			const tw = px - pad * 2 - 6;
			ctx.fillText(this.clip(this.hostOfI[i], tw), x + pad + 5, y + band + f * 0.95 + pad);
			ctx.font = `400 ${f - 1}px "Inter Variable", ui-sans-serif, system-ui, sans-serif`;
			ctx.fillStyle = T.muted;
			let ly = y + band + f * 2.3 + pad;
			const j = this.jobOf[i];
			const line = (t: string) => { ctx.fillText(this.clip(t, tw), x + pad + 5, ly); ly += f * 1.35; };
			line(c === 'busy' && j != null ? `#${j} ${this.jobNames.get(j) ?? ''}`.trim() : c === 'busy' ? 'working' : c === 'use' ? 'in use' : c);
			const rate = this.rateOf[i];
			if (!Number.isNaN(rate) && rate > 0) line(fmtRate(rate));
			// bars: CPU then RAM
			const bar = (label: string, val: number, by: number) => {
				if (Number.isNaN(val)) return;
				const bw = px - pad * 2 - 5, bx = x + pad + 5;
				ctx.fillStyle = T.line;
				ctx.fillRect(bx, by, bw, 4);
				ctx.fillStyle = val > 85 ? T.warn : T.primary;
				ctx.fillRect(bx, by, bw * Math.min(1, val / 100), 4);
				if (px > 100) {
					ctx.fillStyle = T.muted;
					ctx.fillText(`${label} ${Math.round(val)} %`, bx, by - f * 0.7);
				}
			};
			bar('CPU', this.cpu[i], y + px - pad - (px > 100 ? 26 : 14));
			bar('RAM', this.ram[i], y + px - pad - 6);
			if (px > 130) {
				ctx.fillStyle = T.muted;
				ctx.fillText(`${this.loss[i]} lost / 24 h`, x + pad + 5, y + px - pad - 38 - f);
			}
			// job stripe on the left edge
			if (c === 'busy' && j != null) {
				ctx.fillStyle = jobColor(T, this.jobIds.get(j) ?? -1);
				ctx.fillRect(x + 2, y + 5, 3, px - 10);
			}
		}
		ctx.globalAlpha = 1;
	}
	/** text cut to a pixel width with an ellipsis (current font) */
	private clip(t: string, w: number): string {
		if (w <= 8) return '';
		if (this.ctx.measureText(t).width <= w) return t;
		let lo = 0, hi = t.length;
		while (lo < hi) {
			const mid = (lo + hi + 1) >> 1;
			if (this.ctx.measureText(t.slice(0, mid) + '…').width <= w) lo = mid;
			else hi = mid - 1;
		}
		return lo > 0 ? t.slice(0, lo) + '…' : '';
	}

	/** corner map at near zoom: labs, and the visible window */
	private drawMini() {
		this.mini = null;
		if (this.lod !== 'card' || this.w < 360) return;
		const { ctx, tokens: T, view: v, scene: S } = this;
		const mw = 132, s = Math.min(mw / S.w, 84 / S.h), mh = S.h * s, mwid = S.w * s;
		const x = this.w - mwid - 10, y = this.padTop + 8;
		this.mini = { x, y, w: mwid, h: mh, s };
		ctx.globalAlpha = 0.92;
		ctx.fillStyle = T.surface;
		ctx.strokeStyle = T.line;
		ctx.lineWidth = 1;
		ctx.beginPath();
		ctx.roundRect(x - 4, y - 4, mwid + 8, mh + 8, 6);
		ctx.fill();
		ctx.stroke();
		ctx.globalAlpha = 1;
		ctx.fillStyle = T.line;
		for (const l of S.labs) ctx.fillRect(x + l.x * s, y + l.y * s, l.w * s, l.h * s);
		ctx.strokeStyle = T.primary;
		ctx.lineWidth = 1.5;
		const vx = x + (-v.tx / v.k) * s, vy = y + (-v.ty / v.k) * s;
		ctx.strokeRect(vx, vy, (this.w / v.k) * s, (this.h / v.k) * s);
	}

	private drawParticles(now: number) {
		if (this.huge) return;
		const { ctx, tokens: T, particles: P } = this;
		const groups = new Map<number, number[]>();
		for (let i = 0; i < P.n; i++) {
			if (now < P.t0[i]) continue;
			(groups.get(P.job[i] * 2 + P.spec[i]) ?? groups.set(P.job[i] * 2 + P.spec[i], []).get(P.job[i] * 2 + P.spec[i])!).push(i);
		}
		for (const [key, list] of groups) {
			const spec = key & 1, job = (key - spec) / 2;
			ctx.fillStyle = ctx.strokeStyle = jobColor(T, job);
			ctx.beginPath();
			for (const i of list) {
				const u = ease((now - P.t0[i]) / P.dur[i]);
				const x = P.x0[i] + (P.x1[i] - P.x0[i]) * u, y = P.y0[i] + (P.y1[i] - P.y0[i]) * u;
				ctx.moveTo(x + 1.6, y);
				ctx.arc(x, y, 1.6, 0, TAU);
			}
			ctx.globalAlpha = 0.9;
			if (spec) {
				ctx.lineWidth = 0.8;
				ctx.setLineDash([1, 1]);
				ctx.stroke();
				ctx.setLineDash([]);
			} else ctx.fill();
		}
		ctx.globalAlpha = 1;
	}

	private tone(t: Tone) {
		const T = this.tokens;
		return t === 'ok' ? T.ok : t === 'bad' ? T.bad : t === 'warn' ? T.warn : t === 'busy' ? T.primary : T.idle;
	}
	private drawFx(now: number) {
		const { ctx, tokens: T, view: v } = this;
		for (const f of this.fx) {
			const u = (now - f.t0) / f.dur;
			if (u < 0 || u >= 1) continue;
			const at = f.id === 'head' ? this.scene.head : (() => { const i = this.scene.index.get(f.id); return i == null ? null : this.scene.pcs[i]; })();
			if (!at) continue;
			const x = at.x, y = at.y;
			const color = f.fx.type === 'ripple' ? this.tone(f.fx.tone) : f.fx.type === 'glyph' ? (f.fx.glyph === 'moon' ? T.idle : T.warn) : T.ok;
			ctx.globalAlpha = (1 - u) * 0.8;
			ctx.strokeStyle = color;
			ctx.lineWidth = 2 / Math.max(1, v.k * 0.6);
			ctx.beginPath();
			ctx.arc(x, y, 5 + ease(u) * 24, 0, TAU);
			ctx.stroke();
			if (f.fx.type === 'glyph') {
				ctx.globalAlpha = Math.min(1, (1 - u) * 1.6);
				ctx.strokeStyle = color;
				drawGlyph(ctx, f.fx.glyph, x, y - 14 - u * 8, 12);
			}
		}
		ctx.globalAlpha = 1;
	}

	private drawHeadRow(now: number) {
		const { ctx, tokens: T, scene: S } = this;
		const reduce = this.reduce;
		const head = S.head;
		// replication lines
		ctx.lineWidth = 1.4;
		this.shown.forEach((s, i) => {
			const p = S.standby(i);
			const born = this.born.get(s.id);
			const fade = born != null ? Math.min(1, (now - born) / 600) : 1;
			ctx.globalAlpha = (this.lost ? 0.25 : 0.9) * fade;
			ctx.strokeStyle = this.lost ? T.bad : s.lag > 0 ? T.warn : T.ok;
			ctx.setLineDash(this.lost ? [3, 3] : []);
			ctx.beginPath();
			ctx.moveTo(head.x, head.y);
			ctx.lineTo(p.x, p.y);
			ctx.stroke();
		});
		ctx.setLineDash([]);
		// standby nodes
		this.shown.forEach((s, i) => {
			const p = S.standby(i);
			const born = this.born.get(s.id);
			const fade = born != null ? ease(Math.min(1, (now - born) / 600)) : 1;
			const x = p.x, y = p.y;
			ctx.globalAlpha = (this.lost && i > 0 ? 0.5 : 1) * fade;
			this.node(x, y, 10, T.surface, this.lost && i === 0 ? T.warn : s.lag > 0 ? T.warn : T.muted);
			ctx.strokeStyle = this.lost && i === 0 ? T.warn : T.muted;
			drawGlyph(ctx, 'eye', x, y, 11);
		});
		ctx.globalAlpha = 1;
		// the promoted standby glides from its slot into the head spot
		const g = this.glide;
		if (g) {
			const from = this.slotOf(g.id);
			const u = ease(Math.min(1, (now - g.t0) / 600));
			const x = from.x + (head.x - from.x) * u, y = from.y + (head.y - from.y) * u;
			this.node(x, y, 10 + 6 * u, T.surface, T.primary);
			ctx.strokeStyle = T.primary;
			drawGlyph(ctx, 'crown', x, y, 11 + 9 * u);
		}
		// the head (cracked while lost; a fading ghost of the old head while the new one arrives)
		const ghostU = (now - this.ghost) / 650;
		if (this.lost || (ghostU >= 0 && ghostU < 1)) {
			ctx.globalAlpha = this.lost ? 1 : 1 - ghostU;
			this.node(head.x, head.y, 16, T.surface, T.bad);
			ctx.strokeStyle = T.bad;
			drawGlyph(ctx, 'crown', head.x, head.y, 20);
			ctx.beginPath();
			ctx.moveTo(head.x - 3, head.y - 16);
			ctx.lineTo(head.x + 2, head.y - 6);
			ctx.lineTo(head.x - 4, head.y + 1);
			ctx.lineTo(head.x + 3, head.y + 16);
			ctx.lineWidth = 1.6;
			ctx.stroke();
			ctx.globalAlpha = 1;
		} else if (!g || reduce) {
			this.node(head.x, head.y, 16, T.surface, T.primary);
			ctx.strokeStyle = T.primary;
			drawGlyph(ctx, 'crown', head.x, head.y, 20);
		}
		// a slot waiting for a recruit is drawn by the legend / HeadTopology; here only real standbys
		this.drawLabHeads();
	}

	/** Lab heads: a small hexagon on the top edge of the lab box, a link up to the root, and a ring that fills as the lease is worked off. */
	private drawLabHeads() {
		const { ctx, tokens: T, scene: S } = this;
		if (!this.labHeads.size) return;
		const R = 6.5, RING = 9.5;
		for (const l of S.labs) {
			const h = this.labHeads.get(l.name);
			if (!h) continue;
			const x = l.x + l.w / 2, y = l.y;
			const tone = h.cut ? T.warn : h.drain ? T.idle : T.primary;
			ctx.lineWidth = 1.2;
			ctx.globalAlpha = 0.7;
			ctx.strokeStyle = tone;
			ctx.setLineDash(h.cut ? [3, 3] : []);
			ctx.beginPath();
			ctx.moveTo(S.head.x, S.head.y + 16);
			ctx.quadraticCurveTo(S.head.x, (S.head.y + y) / 2 + 20, x, y - RING);
			ctx.stroke();
			ctx.setLineDash([]);
			ctx.globalAlpha = 1;
			this.node(x, y, R, T.surface, tone);
			ctx.strokeStyle = tone;
			ctx.beginPath();
			ctx.arc(x, y, RING, 0, TAU);
			ctx.globalAlpha = 0.25;
			ctx.stroke();
			ctx.globalAlpha = 1;
			if (h.progress != null) {
				ctx.lineWidth = 2;
				ctx.beginPath();
				ctx.arc(x, y, RING, -Math.PI / 2, -Math.PI / 2 + TAU * Math.max(0.02, h.progress));
				ctx.stroke();
			}
			drawGlyph(ctx, 'crown', x, y, 7);
		}
	}
	private slotOf(id: string) {
		const i = this.shown.findIndex((s) => s.id === id);
		return this.scene.standby(Math.max(0, i));
	}
	/** the head and standbys are hexagons */
	private node(x: number, y: number, r: number, fill: string, stroke: string) {
		const { ctx } = this;
		ctx.beginPath();
		for (let i = 0; i < 6; i++) {
			const a = (Math.PI / 3) * i - Math.PI / 2;
			ctx[i ? 'lineTo' : 'moveTo'](x + Math.cos(a) * r, y + Math.sin(a) * r);
		}
		ctx.closePath();
		ctx.fillStyle = fill;
		ctx.fill();
		ctx.strokeStyle = stroke;
		ctx.lineWidth = 2;
		ctx.stroke();
	}

	private drawLabels(now: number) {
		const { ctx, tokens: T, view: v, scene: S } = this;
		ctx.font = '500 11px "Inter Variable", ui-sans-serif, system-ui, sans-serif';
		ctx.textBaseline = 'middle';
		const sx = (x: number) => x * v.k + v.tx, sy = (y: number) => y * v.k + v.ty;
		ctx.textAlign = 'left';
		const W = this.w, H = this.h;
		for (const l of S.labs) {
			const px = l.w * v.k, py = 22 * v.k;
			const x = sx(l.x), y = sy(l.y);
			if (x > W || y > H || x + px < 0 || y + l.h * v.k < 0 || px < 28) continue;
			const st = this.labStats.get(l.id) ?? { busy: 0, awake: 0, total: l.count, rate: 0, share: 0 };
			const asleep = this.huge ? ` · ${this.labAsleep(l.id)} asleep` : '';
			const cands = [
				`${l.name}  ${st.busy}/${st.awake}/${st.total} busy/awake/total  ${st.rate > 0 ? fmtRate(st.rate) : '0 tasks/s'}  ${Math.round(st.share * 100)} % of work`,
				`${l.name}  ${st.busy}/${st.awake}/${st.total}  ${st.rate > 0 ? fmtRate(st.rate) : ''}`.trim(),
				`${l.name}  ${st.busy}/${st.awake}/${st.total}`,
				l.name
			];
			if (this.huge) cands.unshift(`${l.name} · ${st.total} PCs${asleep} · ${st.rate > 0 ? fmtRate(st.rate) : '0'}`);
			const room = px - 14;
			const txt = cands.find((c) => ctx.measureText(c).width <= room) ?? this.clip(l.name, room);
			if (!txt) continue;
			const inStrip = py >= 15;
			const ty = inStrip ? y + py / 2 : y + 8;
			if (!inStrip) {
				// zoomed far out: the label rides on a small pill so it never fights the PCs
				const w = ctx.measureText(txt).width + 8;
				ctx.globalAlpha = 0.85;
				ctx.fillStyle = T.surface;
				ctx.fillRect(x + 4, y + 1, w, 14);
				ctx.globalAlpha = 1;
			}
			ctx.fillStyle = T.fg;
			ctx.fillText(txt, x + (inStrip ? 8 : 8), ty);
		}
		// head caption, term counter (rolls on a failover) and standby names
		const head = S.head;
		// the head's name and term sit to its left, so they never meet the lab strips below
		ctx.textAlign = 'right';
		const hx = sx(head.x) - 16 * v.k - 10, hy = sy(head.y);
		ctx.fillStyle = this.lost ? T.bad : T.fg;
		const hostOf = (id: string | null) => (id ? this.input?.nodes[id]?.host ?? id : null);
		ctx.fillText(this.lost ? 'Head lost' : (hostOf(this.headId) ?? 'HEAD'), hx, hy - 7);
		if (this.epoch != null) {
			ctx.fillStyle = T.muted;
			const r = this.roll;
			if (r) {
				const u = Math.min(1, (now - r.t0) / 350);
				ctx.globalAlpha = 1 - u;
				ctx.fillText(`term ${r.from}`, hx, hy + 7 - u * 10);
				ctx.globalAlpha = u;
				ctx.fillText(`term ${this.epoch}`, hx, hy + 7 + (1 - u) * 10);
				ctx.globalAlpha = 1;
			} else ctx.fillText(`term ${this.epoch}`, hx, hy + 7);
		}
		ctx.textAlign = 'center';
		this.shown.forEach((s, i) => {
			const p = S.standby(i);
			ctx.fillStyle = T.muted;
			ctx.fillText(v.k * 64 < 56 ? `${i + 1}` : `${i + 1} ${hostOf(s.id)}`, sx(p.x), sy(p.y + 10) + 11);
		});
	}
	private labAsleep(id: number) {
		let n = 0;
		for (const s of this.scene.pcs) if (s.lab === id && this.cls[this.scene.index.get(s.id)!] === 'asleep') n++;
		return n;
	}
}
