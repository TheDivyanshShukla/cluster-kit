// Turns an uploaded experiment (raw.csv + runs.jsonl + optional sim JSON) into what the notebook shows.
import { parseCsv, parseJsonl, scaling, simSamples, toCsv, type ScaleSeries } from './chart';

export interface Provenance {
	runs: number;
	commits: string[];
	dirty: number;
	heads: string[];
	kinds: string[];
	pcClasses: string[];
	firstMs: number | null;
	lastMs: number | null;
}
export interface Analysis {
	measured: ScaleSeries[];
	sim: ScaleSeries[];
	prov: Provenance;
	runs: Record<string, any>[];
	/** raw.csv rows we could not use (no pcs or rate) */
	skipped: number;
}

const uniq = (v: unknown[]) => [...new Set(v.filter((x) => x !== undefined && x !== null && x !== '').map(String))];

export function analyse(raw_csv: string, runs_jsonl: string, sim: unknown): Analysis {
	const rows = parseCsv(raw_csv);
	const samples = rows.map((r) => ({ pcs: Number(r.pcs), rate: Number(r.hashes_per_s), policy: r.policy ?? '' }));
	const ok = samples.filter((s) => s.pcs > 0 && s.rate > 0);
	const runs = parseJsonl(runs_jsonl);
	const t = runs.flatMap((r) => [Number(r.started_ms), Number(r.finished_ms)]).filter((x) => x > 0);
	return {
		measured: scaling(ok),
		sim: scaling(simSamples(sim)),
		prov: {
			runs: runs.length,
			commits: uniq(runs.map((r) => r.commit)),
			dirty: runs.filter((r) => r.dirty).length,
			heads: uniq(runs.map((r) => r.head)),
			kinds: uniq(runs.map((r) => r.kind)),
			pcClasses: uniq(runs.flatMap((r) => (Array.isArray(r.pcs) ? r.pcs.map((p: any) => p?.class) : []))),
			firstMs: t.length ? Math.min(...t) : null,
			lastMs: t.length ? Math.max(...t) : null
		},
		runs,
		skipped: samples.length - ok.length
	};
}

/** The summary table the figures are drawn from, as CSV. */
export function summaryCsv(a: Analysis): string {
	const simAt = new Map(a.sim.flatMap((s) => s.pts.map((p) => [`${s.policy}|${p.pcs}`, p] as const)));
	const f = (x: number) => (Number.isFinite(x) ? +x.toPrecision(5) : '');
	return toCsv(
		['policy', 'pcs', 'repeats', 'rate_per_s', 'speedup', 'speedup_min', 'speedup_max', 'efficiency', 'efficiency_min', 'efficiency_max', 'karp_flatt', 'sim_speedup'],
		a.measured.flatMap((s) => s.pts.map((p) => [s.policy, p.pcs, p.repeats, f(p.rate), f(p.S), f(p.Slo), f(p.Shi), f(p.E), f(p.Elo), f(p.Ehi), f(p.KF), f(simAt.get(`${s.policy}|${p.pcs}`)?.S ?? NaN)]))
	);
}

/** A caption ready to paste under the figure. */
export function caption(name: string, repeats: number, a: Analysis, virtual: boolean): string {
	const pts = a.measured.flatMap((s) => s.pts);
	const pcs = pts.map((p) => p.pcs);
	const lo = Math.min(...pcs), hi = Math.max(...pcs);
	const pol = a.measured.map((s) => s.policy).filter(Boolean);
	const top = a.measured.map((s) => ({ s, p: s.pts.at(-1)! })).filter((x) => x.p);
	const best = top.sort((x, y) => y.p.E - x.p.E)[0];
	const commit = a.prov.commits.length ? `, commit ${a.prov.commits.join('/')}${a.prov.dirty ? ' (uncommitted changes)' : ''}` : '';
	return [
		`Scaling of ${name} on ${lo} to ${hi} ${virtual ? 'virtual ' : ''}PCs${pol.length ? ` (policies: ${pol.join(', ')})` : ''}.`,
		`Points are the median of ${repeats} repeats; bars show the minimum to maximum.`,
		a.sim.length ? 'Dashed lines are the simulator prediction.' : '',
		best ? `At ${best.p.pcs} PCs the ${best.s.policy ? best.s.policy + ' policy reaches ' : ''}speed-up is ${best.p.S.toFixed(1)}x (efficiency ${(best.p.E * 100).toFixed(1)} %).` : '',
		`Cluster Kit${commit}.`
	].filter(Boolean).join(' ');
}
