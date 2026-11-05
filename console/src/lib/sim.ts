// Simulation Lab helpers (SIM-05): presets, limits, and one place that turns whatever POST /api/v1/sim returns
// (the JSON `ck sim` writes: summary + timeline) into stats and chart rows. Pure, tested in sim.test.ts.
import { parseCsv } from './chart';

/** server caps (docs/API.md): checked here too so the answer is instant */
export const SIM_MAX_NODES = 20_000;
export const SIM_MAX_HOURS = 48;

export type Scenario = Record<string, any>;
// copies of examples/sim/*.json (the page cannot read the repo) and a day-with-eco one
export const PRESETS: { id: string; label: string; note: string; scenario: Scenario }[] = [
	{
		id: 'lab-day', label: 'Four labs, standby head', note: '200 PCs in 4 labs, the head fails at 30 minutes',
		scenario: {
			nodes: [
				{ count: 50, rate: 2e6, jitter: 0.3, mtbf_s: 3600, down_s: 180, lab: 'Lab1' },
				{ count: 50, rate: 2e6, jitter: 0.3, mtbf_s: 3600, down_s: 180, lab: 'Lab2' },
				{ count: 50, rate: 1.2e6, jitter: 0.4, mtbf_s: 1800, down_s: 300, lab: 'Lab3' },
				{ count: 50, rate: 3e6, jitter: 0.2, mtbf_s: 7200, down_s: 120, lab: 'Lab4' }
			],
			job: { n: 3.5e12, policy: 'adaptive' }, architecture: 'standby', head_failure: { at_s: 1800, recovery_s: 6 }, seed: 1, sample_s: 10
		}
	},
	{
		id: 'arch-10k', label: '10,000 PCs with relays', note: 'five labs of 2,000, relays per lab',
		scenario: {
			nodes: ['A', 'B', 'C', 'D', 'E'].map((lab) => ({ count: 2000, rate: 1e6, jitter: 0.2, mtbf_s: 86400, down_s: 120, lab })),
			job: { n: 6e12, policy: 'adaptive' }, architecture: 'relays+standby', relays: { per_lab: true, batch: 20, extra_latency_ms: 0.3 },
			head_failure: { at_s: 300, recovery_s: 6 }, seed: 1, sample_s: 5
		}
	},
	{
		id: 'eco-day', label: 'A day with eco sleep', note: '200 PCs, a class booking, one urgent job',
		scenario: {
			day: { hours: 24, tick_s: 10, sample_s: 300, seed: 1, efficiency: 0.97, chunk_s: 1 },
			pcs: [{ count: 200, rate: 23.3e6, speed_cv: 0.03, run_cv: 0.005, mtbf_s: 0, down_s: 180, threads: 12 }],
			power: { idle_w: 25, busy_w: 65, sleep_w: 2, boot_w: 65, interactive_w: 45, wake_s: 45 },
			eco: { enabled: true, idle_sleep_s: 600, prewake_s: 120, keep_awake: 4 },
			reservations: [{ start_h: 9, end_h: 11, first_pc: 0, pcs: 60 }],
			jobs: [{ name: 'night batch', arrive_h: 1, work: 2e11, priority: 4, max_pcs: 0, placement: 'spread' }],
			head: { mtbf_s: 0, standby: true, recovery_s: 5.7, restart_s: 600 }
		}
	}
];

export const isDay = (s: Scenario) => s != null && typeof s === 'object' && 'day' in s;
export const totalPcs = (s: Scenario): number =>
	((isDay(s) ? s.pcs : s.nodes) as { count?: number }[] | undefined)?.reduce((a, g) => a + (Number(g.count) || 1), 0) ?? 0;
export const hoursOf = (s: Scenario): number => (isDay(s) ? Number(s.day?.hours ?? 24) : 0);

/** null when the scenario may run, else the reason in words */
export function checkScenario(s: unknown): string | null {
	if (!s || typeof s !== 'object' || Array.isArray(s)) return 'The scenario must be a JSON object.';
	const sc = s as Scenario;
	const groups = isDay(sc) ? sc.pcs : sc.nodes;
	if (groups !== undefined && !Array.isArray(groups)) return isDay(sc) ? '"pcs" must be a list.' : '"nodes" must be a list.';
	if (totalPcs(sc) > SIM_MAX_NODES) return `At most ${SIM_MAX_NODES.toLocaleString('en')} PCs per run (this has ${totalPcs(sc).toLocaleString('en')}).`;
	if (hoursOf(sc) > SIM_MAX_HOURS) return `At most ${SIM_MAX_HOURS} simulated hours per run.`;
	if (!isDay(sc) && !(Number(sc.job?.n) > 0)) return 'Give the job a size: "job": {"n": ...}.';
	return null;
}

export interface SimStat { label: string; value: string; hint?: string }
export interface SimView {
	kind: 'job' | 'day';
	headline: string;
	stats: SimStat[];
	/** time axis unit of the rows' `t` */
	rows: Record<string, number>[];
	jobs: { name: string; wait_s: number | null; makespan_s: number | null; peak_pcs: number }[];
}

const JOB_COLS = ['t', 'done', 'rate', 'busy', 'connected', 'head'];
const DAY_COLS = ['t', 'asleep', 'booting', 'idle', 'busy', 'class', 'watts'];
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const n = (v: unknown, d = 0): number => num(v) ?? d;
const fmt = (v: number, d = 1) => v.toLocaleString('en', { maximumFractionDigits: d });
export function words(sec: number): string {
	if (sec < 120) return `${fmt(sec)} s`;
	if (sec < 7200) return `${fmt(sec / 60)} min`;
	return `${fmt(sec / 3600, 2)} h`;
}

function rowsOf(raw: unknown, cols: string[]): Record<string, number>[] {
	if (typeof raw === 'string') raw = parseCsv(raw);
	if (!Array.isArray(raw)) return [];
	return raw.map((r) => {
		if (Array.isArray(r)) return Object.fromEntries(cols.map((c, i) => [c, n(r[i])]));
		const o = r as Record<string, unknown>;
		const out: Record<string, number> = {};
		for (const [k, v] of Object.entries(o)) {
			const x = typeof v === 'string' ? Number(v) : v;
			const key = { t_s: 't', rate_per_s: 'rate', busy_nodes: 'busy', connected_nodes: 'connected', head_busy_frac: 'head', awake_idle: 'idle', power_w: 'watts' }[k] ?? k;
			if (typeof x === 'number' && Number.isFinite(x)) out[key] = x;
		}
		return out;
	});
}

/** Accepts {summary, series|timeline}, the flat summary.json with `_timeline`, or a bare summary. Never throws. */
export function normalizeSim(raw: unknown): SimView {
	const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
	const sm = (r.summary && typeof r.summary === 'object' ? r.summary : r) as Record<string, unknown>;
	const series = r.series ?? r.timeline ?? r._timeline ?? sm._timeline ?? sm.timeline;
	const day = num(sm.kwh) != null;
	const rows = rowsOf(series, day ? DAY_COLS : JOB_COLS);
	if (!day) {
		const fin = sm.finished === true;
		const end = n(sm.end_s);
		const eff = n(sm.efficiency_pct);
		return {
			kind: 'job', rows, jobs: [],
			headline: fin ? `Finished in ${words(n(sm.makespan_s, end))}, ${fmt(eff)} % of ideal (${words(n(sm.ideal_s))})` : `Not finished at ${words(end)}: ${fmt(100 * n(sm.done) / Math.max(1, n(sm.n, 1)), 0)} % done`,
			stats: [
				{ label: 'Makespan', value: fin ? words(n(sm.makespan_s, end)) : 'unfinished', hint: `ideal ${words(n(sm.ideal_s))}` },
				{ label: 'Efficiency', value: `${fmt(eff)} %` },
				{ label: 'Tasks', value: fmt(n(sm.tasks), 0), hint: `${fmt(n(sm.tasks_per_s), 0)} per second` },
				{ label: 'Head busy', value: `${fmt(n(sm.head_utilisation_pct))} %`, hint: 'share of time dispatching' },
				{ label: 'PC failures', value: fmt(n(sm.node_failures), 0), hint: `${fmt(n(sm.recovered_work), 0)} items redone` },
				{ label: 'Throughput', value: `${fmt(n(sm.throughput_per_s), 0)} /s` }
			]
		};
	}
	const jobs = (Array.isArray(sm.jobs) ? sm.jobs : []).map((j: any) => ({ name: String(j?.name ?? ''), wait_s: num(j?.wait_s), makespan_s: num(j?.makespan_s), peak_pcs: n(j?.peak_pcs) }));
	return {
		kind: 'day', rows, jobs,
		headline: `${fmt(n(sm.kwh))} kWh over ${fmt(n(sm.hours, 24), 0)} h, ${fmt(n(sm.wakes), 0)} wakes`,
		stats: [
			{ label: 'Energy', value: `${fmt(n(sm.kwh))} kWh` },
			{ label: 'Busy / idle', value: `${fmt(n(sm.kwh_busy))} / ${fmt(n(sm.kwh_idle))} kWh` },
			{ label: 'Sleep / boot', value: `${fmt(n(sm.kwh_sleep))} / ${fmt(n(sm.kwh_boot))} kWh` },
			{ label: 'Wakes', value: fmt(n(sm.wakes), 0) },
			{ label: 'PC failures', value: fmt(n(sm.node_failures), 0), hint: `${fmt(n(sm.head_failures), 0)} head failures` },
			{ label: 'Class delay', value: words(n(sm.class_delay_max_s)), hint: 'longest wait for a booked PC' }
		]
	};
}
