// A 30 minute ring of one-second samples for the scrubber sparkline, filled from the cluster feed while the console is open.
import type { Worker } from '../api/schema';

export type Sample = { t: number; busy: number; tasks: number; pcs: number };
export const SPAN_MS = 30 * 60_000;

export class History {
	samples: Sample[] = [];
	version = 0;
	private prev = new Map<string, number>();
	/** Record one snapshot; `tasks` is the chunk-count increase since the previous one, per second. */
	push(workers: Pick<Worker, 'node_id' | 'id' | 'chunks' | 'busy'>[], t: number) {
		let d = 0, busy = 0;
		const seen = new Map<string, number>();
		for (const w of workers) {
			const k = w.node_id ?? `w${w.id}`;
			seen.set(k, w.chunks);
			const p = this.prev.get(k);
			if (p != null && w.chunks > p) d += w.chunks - p;
			if (w.busy) busy++;
		}
		const last = this.samples[this.samples.length - 1];
		const dt = last ? Math.max(0.2, (t - last.t) / 1000) : 1;
		this.prev = seen;
		if (last && t - last.t < 400) return; // one sample per ~second at most
		this.samples.push({ t, busy, tasks: d / dt, pcs: workers.length });
		const cut = t - SPAN_MS;
		if (this.samples[0].t < cut) this.samples = this.samples.filter((s) => s.t >= cut);
		this.version++;
	}
}
export const history = new History();

/** Average samples into n equal buckets between from and to (empty buckets repeat the previous value). */
export function bucket(samples: Sample[], from: number, to: number, n: number): { busy: number[]; tasks: number[] } {
	const busy = new Array<number>(n).fill(0), tasks = new Array<number>(n).fill(0), cnt = new Array<number>(n).fill(0);
	const step = (to - from) / n;
	for (const s of samples) {
		if (s.t < from || s.t > to) continue;
		const i = Math.min(n - 1, Math.floor((s.t - from) / step));
		busy[i] += s.busy;
		tasks[i] += s.tasks;
		cnt[i]++;
	}
	for (let i = 0; i < n; i++) {
		if (cnt[i]) {
			busy[i] /= cnt[i];
			tasks[i] /= cnt[i];
		} else if (i) {
			busy[i] = busy[i - 1];
			tasks[i] = tasks[i - 1];
		}
	}
	return { busy, tasks };
}
