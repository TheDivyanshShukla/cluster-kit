// Task particles. There is no per-chunk event on the stream, so a particle is drawn per chunk-count increase seen between
// two cluster snapshots and spread over the next second. A hard cap keeps high task rates readable as density.
export const MAX_PARTICLES = 300;
export type Plan = { id: string; delay: number; dir: 'out' | 'back'; job: number | null; spec: boolean };

/**
 * Turn chunk deltas into at most `room` particles. Each chunk is a dispatch (head to PC) followed by a result (PC to head),
 * so two particles per chunk; over the budget, chunks are sampled in proportion.
 */
export function planParticles(deltas: { id: string; n: number; job: number | null }[], room: number, windowMs = 1000, rnd: () => number = Math.random): Plan[] {
	const total = deltas.reduce((a, d) => a + Math.max(0, d.n), 0);
	if (total <= 0 || room < 2) return [];
	const keep = Math.min(1, room / (2 * total));
	const out: Plan[] = [];
	for (const d of deltas) {
		const want = Math.max(0, d.n) * keep;
		let c = Math.floor(want) + (rnd() < want - Math.floor(want) ? 1 : 0);
		while (c-- > 0 && out.length + 2 <= room) {
			const delay = rnd() * windowMs;
			out.push({ id: d.id, delay, dir: 'out', job: d.job, spec: false });
			out.push({ id: d.id, delay: delay + 350 + rnd() * 300, dir: 'back', job: d.job, spec: false });
		}
	}
	return out;
}

/** Fixed-size particle pool with parallel arrays (no allocation per frame). Positions come from the endpoints, never from physics. */
export class Particles {
	n = 0;
	x0 = new Float32Array(MAX_PARTICLES);
	y0 = new Float32Array(MAX_PARTICLES);
	x1 = new Float32Array(MAX_PARTICLES);
	y1 = new Float32Array(MAX_PARTICLES);
	t0 = new Float64Array(MAX_PARTICLES);
	dur = new Float32Array(MAX_PARTICLES);
	job = new Int8Array(MAX_PARTICLES);
	spec = new Uint8Array(MAX_PARTICLES);
	get room() {
		return MAX_PARTICLES - this.n;
	}
	spawn(x0: number, y0: number, x1: number, y1: number, t0: number, dur: number, job: number | null, spec = false): boolean {
		if (this.n >= MAX_PARTICLES) return false;
		const i = this.n++;
		this.x0[i] = x0; this.y0[i] = y0; this.x1[i] = x1; this.y1[i] = y1;
		this.t0[i] = t0; this.dur[i] = dur; this.job[i] = job ?? -1; this.spec[i] = spec ? 1 : 0;
		return true;
	}
	/** Drop finished particles (swap-remove). */
	prune(now: number) {
		for (let i = 0; i < this.n; ) {
			if (now >= this.t0[i] + this.dur[i]) {
				const l = --this.n;
				if (i !== l) {
					this.x0[i] = this.x0[l]; this.y0[i] = this.y0[l]; this.x1[i] = this.x1[l]; this.y1[i] = this.y1[l];
					this.t0[i] = this.t0[l]; this.dur[i] = this.dur[l]; this.job[i] = this.job[l]; this.spec[i] = this.spec[l];
				}
			} else i++;
		}
	}
	clear() {
		this.n = 0;
	}
}
