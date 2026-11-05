// Tiny chart helper (docs/DESIGN.md section 9): scales, ticks, CSV, and the scaling maths for the experiment notebook.
// Pure functions are tested in chart.test.ts; the last two (download, copyPng) touch the DOM.

/** Okabe-Ito order: colour-blind safe hues for series that need one each (compare). */
export const OKABE = ['#0072B2', '#E69F00', '#009E73', '#CC79A7', '#56B4E9', '#D55E00', '#F0E442', '#888888'];

export function niceTicks(lo: number, hi: number, n = 5): number[] {
	if (!(hi > lo)) return [lo];
	const raw = (hi - lo) / Math.max(1, n);
	const mag = 10 ** Math.floor(Math.log10(raw));
	const step = ([1, 2, 2.5, 5, 10].find((m) => m * mag >= raw) ?? 10) * mag;
	const out: number[] = [];
	for (let v = Math.ceil(lo / step) * step; v <= hi + step * 1e-9; v += step) out.push(+v.toPrecision(12));
	return out;
}

/** Short axis/hover number: 1.2k, 3.4M, 0.25. */
export function fmtNum(v: number, digits = 3): string {
	if (!Number.isFinite(v)) return '—';
	const a = Math.abs(v);
	if (a >= 1e9) return `${+(v / 1e9).toPrecision(digits)}G`;
	if (a >= 1e6) return `${+(v / 1e6).toPrecision(digits)}M`;
	if (a >= 1e4) return `${+(v / 1e3).toPrecision(digits)}k`;
	if (a === 0) return '0';
	return String(+v.toPrecision(digits));
}

const cell = (v: string | number) => {
	const s = String(v);
	return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
};
export const toCsv = (header: string[], rows: (string | number)[][]): string => [header, ...rows].map((r) => r.map(cell).join(',')).join('\n') + '\n';

/** Header-keyed rows from a simple CSV (quotes allowed, no embedded newlines inside quotes). */
export function parseCsv(text: string): Record<string, string>[] {
	const lines = text.split(/\r?\n/).filter((l) => l.trim() !== '');
	const split = (l: string) => {
		const out: string[] = [];
		let cur = '', q = false;
		for (let i = 0; i < l.length; i++) {
			const c = l[i]!;
			if (q) c === '"' ? (l[i + 1] === '"' ? ((cur += '"'), i++) : (q = false)) : (cur += c);
			else if (c === '"') q = true;
			else if (c === ',') (out.push(cur), (cur = ''));
			else cur += c;
		}
		return [...out, cur];
	};
	const head = split(lines[0] ?? '');
	return lines.slice(1).map((l) => Object.fromEntries(split(l).map((v, i) => [head[i] ?? `c${i}`, v])));
}

export function parseJsonl(text: string): Record<string, any>[] {
	const out: Record<string, any>[] = [];
	for (const l of text.split('\n')) {
		if (!l.trim()) continue;
		try {
			out.push(JSON.parse(l));
		} catch {
			// a torn line is skipped; the rest is still provenance
		}
	}
	return out;
}

export const median = (v: number[]): number => {
	if (!v.length) return NaN;
	const s = [...v].sort((a, b) => a - b), m = s.length >> 1;
	return s.length % 2 ? s[m]! : (s[m - 1]! + s[m]!) / 2;
};

export interface ScalePoint {
	pcs: number;
	repeats: number;
	/** median rate (items/s) and range over the repeats */
	rate: number;
	S: number; Slo: number; Shi: number;
	E: number; Elo: number; Ehi: number;
	/** Karp-Flatt serial fraction; NaN at one PC */
	KF: number; KFlo: number; KFhi: number;
}
export interface ScaleSeries {
	policy: string;
	pts: ScalePoint[];
}

/** Karp-Flatt experimentally determined serial fraction. */
export const karpFlatt = (S: number, p: number): number => (p > 1 ? (1 / S - 1 / p) / (1 - 1 / p) : NaN);

/** Speed-up, efficiency and Karp-Flatt per PC count, per policy, from (pcs, rate) samples. Baseline = the smallest PC count's median. */
export function scaling(samples: { pcs: number; rate: number; policy?: string }[]): ScaleSeries[] {
	const byPol = new Map<string, Map<number, number[]>>();
	for (const s of samples) {
		if (!(s.pcs > 0) || !(s.rate > 0)) continue;
		const m = byPol.get(s.policy ?? '') ?? new Map<number, number[]>();
		m.set(s.pcs, [...(m.get(s.pcs) ?? []), s.rate]);
		byPol.set(s.policy ?? '', m);
	}
	return [...byPol].map(([policy, m]) => {
		const ps = [...m.keys()].sort((a, b) => a - b);
		const p0 = ps[0]!, base = median(m.get(p0)!) / p0; // rate of one PC
		const pts = ps.map((pcs) => {
			const r = m.get(pcs)!, S = (x: number) => x / base, E = (x: number) => S(x) / pcs;
			const lo = Math.min(...r), hi = Math.max(...r), med = median(r);
			// efficiency and serial fraction fall as the rate falls: lo/hi swap for Karp-Flatt
			return {
				pcs, repeats: r.length, rate: med,
				S: S(med), Slo: S(lo), Shi: S(hi), E: E(med), Elo: E(lo), Ehi: E(hi),
				KF: karpFlatt(S(med), pcs), KFlo: karpFlatt(S(hi), pcs), KFhi: karpFlatt(S(lo), pcs)
			};
		});
		return { policy, pts };
	});
}

/** The sim prediction: rows shaped like docs/sim/e1_scaling.csv (pcs, throughput_per_s, optional policy), bare or as {rows}. */
export function simSamples(sim: unknown): { pcs: number; rate: number; policy: string }[] {
	const rows = Array.isArray(sim) ? sim : Array.isArray((sim as { rows?: unknown })?.rows) ? (sim as { rows: unknown[] }).rows : [];
	return rows.flatMap((r: any) => {
		const pcs = Number(r?.pcs), rate = Number(r?.throughput_per_s ?? Number(r?.thr_mhs) * 1e6);
		return pcs > 0 && rate > 0 ? [{ pcs, rate, policy: String(r?.policy ?? '') }] : [];
	});
}

export function download(name: string, text: string, mime = 'text/csv') {
	const url = URL.createObjectURL(new Blob([text], { type: mime }));
	const a = Object.assign(document.createElement('a'), { href: url, download: name });
	a.click();
	setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Copy an SVG chart to the clipboard as a PNG. CSS variables are resolved to real colours first. */
export async function copyPng(svg: SVGSVGElement, scale = 2): Promise<void> {
	const probe = document.createElement('i');
	svg.parentElement!.appendChild(probe);
	let src = new XMLSerializer().serializeToString(svg);
	src = src.replace(/var\((--[\w-]+)\)/g, (_, v) => ((probe.style.color = `var(${v})`), getComputedStyle(probe).color));
	probe.remove();
	const { width, height } = svg.getBoundingClientRect();
	const img = new Image();
	await new Promise<void>((ok, no) => {
		img.onload = () => ok();
		img.onerror = () => no(new Error('render'));
		img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(src);
	});
	const c = document.createElement('canvas');
	c.width = width * scale;
	c.height = height * scale;
	const g = c.getContext('2d')!;
	g.fillStyle = getComputedStyle(document.body).backgroundColor;
	g.fillRect(0, 0, c.width, c.height);
	g.drawImage(img, 0, 0, c.width, c.height);
	const blob = await new Promise<Blob | null>((ok) => c.toBlob(ok, 'image/png'));
	if (!blob) throw new Error('png');
	await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]);
}
