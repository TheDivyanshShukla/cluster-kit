// Canvas layers (docs/DESIGN.md section 9): what a PC's fill means, its legend and scale. Pure, tested in layers.test.ts.
export const LAYERS = ['status', 'load', 'throughput', 'job', 'power', 'reliability', 'fitness'] as const;
export type Layer = (typeof LAYERS)[number];
export const LAYER_LABEL: Record<Layer, string> = { status: 'Status', load: 'Load', throughput: 'Throughput', job: 'Job', power: 'Power', reliability: 'Reliability', fitness: 'Head fitness' };
export const isLayer = (s: string | null | undefined): s is Layer => !!s && (LAYERS as readonly string[]).includes(s);

/** Viridis, 9 stops: sequential and colour-blind safe, readable on light and dark surfaces. */
const RAMP = ['#440154', '#472d7b', '#3b528b', '#2c728e', '#21918c', '#28ae80', '#5ec962', '#addc30', '#fde725'];
export const HEAT_STEPS = 12;
const hex = (h: string, i: number) => parseInt(h.slice(1 + i * 2, 3 + i * 2), 16);
/** Colour for t in [0,1], quantised to HEAT_STEPS so the canvas can batch PCs per colour. */
export function heat(t: number): string {
	const u = Math.round(Math.min(1, Math.max(0, t)) * (HEAT_STEPS - 1)) / (HEAT_STEPS - 1) * (RAMP.length - 1);
	const i = Math.min(RAMP.length - 2, Math.floor(u)), f = u - i;
	const c = [0, 1, 2].map((k) => Math.round(hex(RAMP[i], k) * (1 - f) + hex(RAMP[i + 1], k) * f));
	return `rgb(${c[0]} ${c[1]} ${c[2]})`;
}
export const HEAT_GRADIENT = `linear-gradient(90deg, ${RAMP.join(', ')})`;

/** Normalised value (0..1) of a layer for one PC, or null when unknown. `max` is the fleet's best/largest. */
export type Metrics = { cpu: number | null; rate: number | null; losses: number; score: number | null };
export function layerValue(layer: Layer, m: Metrics, max: { rate: number; losses: number }): number | null {
	switch (layer) {
		case 'load': return m.cpu == null ? null : m.cpu / 100;
		case 'throughput': return m.rate == null ? null : max.rate > 0 ? m.rate / max.rate : 0;
		case 'reliability': return Math.min(1, m.losses / Math.max(3, max.losses));
		case 'fitness': return m.score == null ? null : m.score / 100;
		default: return null;
	}
}

export type Legend = { kind: 'scale'; lo: string; hi: string; unit: string; note?: string } | { kind: 'items'; items: { label: string; swatch: 'busy' | 'idle' | 'warn' | 'bad' | 'ok' | 'asleep' }[] } | { kind: 'jobs' };
export function legendFor(layer: Layer, max: { rate: number; losses: number }, rateText: (r: number) => string): Legend {
	switch (layer) {
		case 'load': return { kind: 'scale', lo: '0', hi: '100', unit: '% CPU busy' };
		case 'throughput': return { kind: 'scale', lo: '0', hi: rateText(max.rate), unit: 'of the fleet\'s best' };
		case 'reliability': return { kind: 'scale', lo: '0', hi: String(Math.max(3, max.losses)), unit: 'losses in 24 h' };
		case 'fitness': return { kind: 'scale', lo: '0', hi: '100', unit: 'head score', note: 'Needs the operator role' };
		case 'job': return { kind: 'jobs' };
		case 'power': return { kind: 'items', items: [{ label: 'awake', swatch: 'ok' }, { label: 'asleep', swatch: 'asleep' }, { label: 'waking', swatch: 'warn' }, { label: 'lost', swatch: 'bad' }] };
		default: return { kind: 'items', items: [] };
	}
}

/** Level of detail from the on-screen size of one PC cell (docs/DESIGN.md section 9: under 6 px a point, over 40 px a card). */
export type Lod = 'point' | 'square' | 'card';
export const lodOf = (pxPerPc: number): Lod => (pxPerPc < 6 ? 'point' : pxPerPc > 40 ? 'card' : 'square');

/** URL state `view=k,cx,cy` (zoom and world centre). */
export function encodeView(k: number, cx: number, cy: number): string {
	return [k, cx, cy].map((n) => String(Math.round(n * 100) / 100)).join(',');
}
export function decodeView(s: string | null): { k: number; cx: number; cy: number } | null {
	const p = (s ?? '').split(',').map(Number);
	return p.length === 3 && p.every(Number.isFinite) && p[0] > 0.02 && p[0] <= 20 ? { k: p[0], cx: p[1], cy: p[2] } : null;
}
