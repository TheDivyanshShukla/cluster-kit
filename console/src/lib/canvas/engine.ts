// The canvas renderer: owns the layout, view, particles and effects, and draws only when something changed or moves.
// The Svelte component feeds it data (update / setEvent) and pointer input; nothing here touches the DOM except the canvas.
import type { EventRow, HaStatus, JobRow, LabRow, NodeRow, Worker } from '../api/schema';
import { toItems } from '../map';
import { effectFor, hostIndex, schedule, type HostIndex, type Live, type Tone } from './effects';
import { jobColor, readTokens, type Tokens } from './colors';
import { drawGlyph } from './glyphs';
import { fitView, hitPc, layoutScene, toWorld, type PcIn, type Scene, type View } from './layout';
import { Particles, planParticles } from './particles';
import type { NodeSnap } from './replay';
import { attrsOf, classOf, CLASSES, jobSlots, makeJobFinder, type Cls } from './state';
import type { Attrs, Matcher } from './select';

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
};
type Group = { cls: Cls; slot: number; dim: boolean; idx: number[] };
const TAU = Math.PI * 2;
const ease = (u: number) => 1 - (1 - u) ** 3;

export class LiveEngine {
	ctx: CanvasRenderingContext2D;
	tokens!: Tokens;
	scene: Scene = layoutScene([], []);
	view: View = { k: 1, tx: 0, ty: 0 };
	userView = false;
	/** screen px kept free at the bottom for the legend */
	padBottom = 0;
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
		this.view = this.fit();
		this.dirty = true;
	}
	private fit() {
		return fitView(this.scene, this.w, Math.max(60, this.h - this.padBottom));
	}
	setPad(px: number) {
		if (px === this.padBottom) return;
		this.padBottom = px;
		if (!this.userView) this.view = this.fit();
		this.dirty = true;
	}
	zoomAt(sx: number, sy: number, f: number) {
		const k = Math.min(8, Math.max(0.1, this.view.k * f));
		const wp = toWorld(this.view, sx, sy);
		this.view = { k, tx: sx - wp.x * k, ty: sy - wp.y * k };
		this.userView = true;
		this.dirty = true;
	}
	panBy(dx: number, dy: number) {
		this.view = { ...this.view, tx: this.view.tx + dx, ty: this.view.ty + dy };
		this.userView = true;
		this.dirty = true;
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
		for (const { node, w } of items) {
			const i = this.scene.index.get(node.node_id)!;
			const o = inp.overrides?.[node.node_id];
			const view = o ? { ...node, ...o } : node;
			const c = classOf(view, w);
			this.cls[i] = c;
			this.counts[c]++;
			const a = attrsOf(node, labName.get(node.lab_id ?? -1), c, null, this.scene.pcs[i].r > 4 ? 16 : 8);
			if (c === 'busy') this.jobOf[i] = finder(a);
			a.job = this.jobOf[i] == null ? undefined : String(this.jobOf[i]);
			attrs[i] = a;
		}
		this.dim = new Uint8Array(n);
		if (inp.filter) for (let i = 0; i < n; i++) this.dim[i] = inp.filter(attrs[i]) ? 0 : 1;
		this.buildGroups();
		this.setHa(inp, now);
		if (!this.replay) this.ingestChunks(inp.workers, now);
		else this.particles.clear();
		this.dirty = true;
	}
	/** how many PCs match the filter (all when none) */
	get matched() {
		return this.dim.length - this.dim.reduce((a, d) => a + d, 0);
	}
	private buildGroups() {
		const m = new Map<string, Group>();
		for (let i = 0; i < this.cls.length; i++) {
			const cls = this.cls[i];
			const slot = cls === 'busy' ? (this.jobOf[i] == null ? -1 : (this.jobIds.get(this.jobOf[i]!) ?? -1)) : 0;
			const dim = this.dim[i] === 1;
			const key = `${cls}|${slot}|${dim}`;
			(m.get(key) ?? m.set(key, { cls, slot, dim, idx: [] }).get(key)!).idx.push(i);
		}
		this.groups = [...m.values()].sort((a, b) => CLASSES.indexOf(a.cls) - CLASSES.indexOf(b.cls));
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
		return this.particles.n > 0 || this.fx.some((f) => now - f.t0 < f.dur) || this.flashes.size > 0 || !!this.glide || !!this.roll || now - this.ghost < 700 || this.born.size > 0 || (!this.reduce && this.counts.waking > 0);
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
		// lab regions
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
		}
		this.drawHeadRow(now);
		this.drawPcs(now);
		this.drawParticles(now);
		this.drawFx(now);
		// screen-space text on top
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		this.drawLabels(now);
	}

	private drawPcs(now: number) {
		const { ctx, tokens: T, view: v } = this;
		const pulse = this.reduce ? 0.6 : 0.5 + 0.4 * Math.sin(now / 280);
		const px = this.huge ? 1.5 / v.k : 0;
		for (const g of this.groups) {
			ctx.beginPath();
			for (const i of g.idx) {
				const s = this.scene.pcs[i];
				const r = px || s.r;
				ctx.moveTo(s.x + r, s.y);
				ctx.arc(s.x, s.y, r, 0, TAU);
			}
			const a = g.dim ? 0.15 : 1;
			const col = g.cls === 'busy' ? jobColor(T, g.slot) : g.cls === 'lost' ? T.bad : g.cls === 'waking' ? T.warn : T.idle;
			ctx.lineWidth = (g.cls === 'use' ? 1.6 : 1.1) / Math.max(1, v.k * 0.6);
			ctx.setLineDash(g.cls === 'asleep' ? [2, 2] : []);
			switch (g.cls) {
				case 'busy': ctx.globalAlpha = a; ctx.fillStyle = col; ctx.fill(); break;
				case 'lost': ctx.globalAlpha = a; ctx.fillStyle = col; ctx.fill(); break;
				case 'waking': ctx.globalAlpha = a * pulse; ctx.fillStyle = col; ctx.fill(); ctx.globalAlpha = a; ctx.strokeStyle = col; ctx.stroke(); break;
				case 'asleep': ctx.globalAlpha = a * 0.35; ctx.fillStyle = T.idle; ctx.fill(); ctx.globalAlpha = a * 0.8; ctx.strokeStyle = T.idle; ctx.stroke(); break;
				case 'use': ctx.globalAlpha = a; ctx.fillStyle = T.surface; ctx.fill(); ctx.strokeStyle = T.warn; ctx.stroke(); break;
				default: ctx.globalAlpha = a; ctx.fillStyle = T.surface; ctx.fill(); ctx.strokeStyle = T.idle; ctx.stroke();
			}
			ctx.setLineDash([]);
			if (g.cls === 'cord') {
				// hatched: a diagonal stroke through the disc
				ctx.beginPath();
				for (const i of g.idx) {
					const s = this.scene.pcs[i];
					const r = (px || s.r) * 0.75;
					ctx.moveTo(s.x - r, s.y + r);
					ctx.lineTo(s.x + r, s.y - r);
				}
				ctx.globalAlpha = a;
				ctx.strokeStyle = T.idle;
				ctx.stroke();
			}
		}
		ctx.globalAlpha = 1;
		// standbys wear a ring; flashed PCs a pulsing primary ring
		if (!this.huge) {
			ctx.lineWidth = 1 / Math.max(1, v.k * 0.6);
			ctx.strokeStyle = T.muted;
			ctx.setLineDash([1.5, 1.5]);
			ctx.beginPath();
			for (const id of this.standbyIds) {
				const i = this.scene.index.get(id);
				if (i == null) continue;
				const s = this.scene.pcs[i];
				ctx.moveTo(s.x + s.r + 2.5, s.y);
				ctx.arc(s.x, s.y, s.r + 2.5, 0, TAU);
			}
			ctx.stroke();
			ctx.setLineDash([]);
		}
		for (const [id, until] of this.flashes) {
			const i = this.scene.index.get(id);
			if (i == null) continue;
			const s = this.scene.pcs[i];
			const u = 1 - (until - now) / 2200;
			ctx.globalAlpha = Math.max(0, 1 - u) * (0.6 + 0.4 * Math.sin(now / 90));
			ctx.strokeStyle = T.primary;
			ctx.lineWidth = 2 / Math.max(1, v.k * 0.6);
			ctx.beginPath();
			ctx.arc(s.x, s.y, s.r + 4 + u * 6, 0, TAU);
			ctx.stroke();
		}
		ctx.globalAlpha = 1;
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
	}
	private slotOf(id: string) {
		const i = this.shown.findIndex((s) => s.id === id);
		return this.scene.standby(Math.max(0, i));
	}
	private node(x: number, y: number, r: number, fill: string, stroke: string) {
		const { ctx } = this;
		ctx.beginPath();
		ctx.arc(x, y, r, 0, TAU);
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
		if (v.k > 0.3) {
			ctx.textAlign = 'left';
			for (const l of S.labs) {
				const asleep = this.huge || l.count > 0 ? l.count : 0;
				ctx.fillStyle = T.muted;
				const sleepers = this.huge ? ` · ${this.labAsleep(l.id)} asleep` : '';
				ctx.fillText(`${l.name} · ${asleep}${this.huge ? ' PCs' : ''}${sleepers}`, sx(l.x + 10), sy(l.y + 12));
			}
		}
		// head caption, term counter (rolls on a failover) and standby names
		const head = S.head;
		ctx.textAlign = 'center';
		const hx = sx(head.x), hy = sy(head.y + 16) + 12;
		ctx.fillStyle = this.lost ? T.bad : T.fg;
		const hostOf = (id: string | null) => (id ? this.input?.nodes[id]?.host ?? id : null);
		ctx.fillText(this.lost ? 'Head lost' : (hostOf(this.headId) ?? 'HEAD'), hx, hy);
		if (this.epoch != null) {
			ctx.fillStyle = T.muted;
			const r = this.roll;
			if (r) {
				const u = Math.min(1, (now - r.t0) / 350);
				ctx.globalAlpha = 1 - u;
				ctx.fillText(`term ${r.from}`, hx, hy + 13 - u * 10);
				ctx.globalAlpha = u;
				ctx.fillText(`term ${this.epoch}`, hx, hy + 13 + (1 - u) * 10);
				ctx.globalAlpha = 1;
			} else ctx.fillText(`term ${this.epoch}`, hx, hy + 13);
		}
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
