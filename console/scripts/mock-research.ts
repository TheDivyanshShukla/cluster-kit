// Wave-12 mock routes (docs/API.md "Research features"): breakdown, alerts, experiments, day report.
// Called by mock-api.ts before it falls through to 404. Deterministic: the same job or day always gives the same numbers.
type Ctx = {
	p: string; m: string; url: URL; me: { name: string; role: string }; RANK: Record<string, number>;
	json: (b: unknown, s?: number) => Response; err: (s: number, c: string, m: string) => Response;
	body: () => Promise<Record<string, any>>; jobs: { id: number; state: string; started_ms: number | null; finished_ms: number | null; total: number }[];
	workers: { node_id: string; host: string }[]; now: () => number; subs: Set<(n: string, d: unknown) => void>;
};
const rnd = (seed: number) => () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32);
const hash = (s: string) => [...s].reduce((a, c) => (a * 31 + c.charCodeAt(0)) >>> 0, 7);

type Alert = { id: number; kind: string; severity: string; node_id: string | null; detail: string; since_ms: number; last_ms: number; active: boolean; acked_by: string | null };
const alerts: Alert[] = [];
let alertSeq = 0;
const mk = (a: Partial<Alert> & Pick<Alert, 'kind' | 'severity' | 'detail'>, ago: number, active = true, dur = 0): Alert => {
	const dup = active && alerts.find((y) => y.active && y.kind === a.kind && y.node_id === (a.node_id ?? null));
	if (dup) return dup;
	const t = Date.now();
	const x: Alert = { id: ++alertSeq, node_id: null, since_ms: t - ago, last_ms: active ? t : t - ago + dur, active, acked_by: null, ...a };
	alerts.push(x);
	return x;
};
const H = 3.6e6;
mk({ kind: 'slow_pc', severity: 'warn', node_id: 'n007', detail: 'pc-07 runs at 41 % of the fleet median (rate 3.1 M/s vs 7.6 M/s, median − 3 MAD = 5.2 M/s)' }, 0.4 * H);
mk({ kind: 'flaky_pc', severity: 'bad', node_id: 'n013', detail: 'pc-13 was lost 4 times in 24 h (limit 3)' }, 2.1 * H);
mk({ kind: 'reserve_at_risk', severity: 'info', detail: 'reserve "lab A exam" holds 6 of 8 PCs asked' }, 0.1 * H);
mk({ kind: 'head_hot', severity: 'warn', node_id: 'n001', detail: 'head CPU 83 % for 60 s (limit 70 %)' }, 5 * H, false, 0.3 * H);
mk({ kind: 'no_standby', severity: 'bad', detail: '0 standbys, target is 2 (for 5 min)' }, 7 * H, false, 1.2 * H);
mk({ kind: 'standby_behind', severity: 'warn', node_id: 'n004', detail: 'pc-04 is 1,420 changes behind (limit 500)' }, 11 * H, false, 0.2 * H);
mk({ kind: 'slow_pc', severity: 'warn', node_id: 'n022', detail: 'pc-22 runs at 55 % of the fleet median' }, 20 * H, false, 2 * H);
mk({ kind: 'flaky_pc', severity: 'warn', node_id: 'n009', detail: 'pc-09 was lost 4 times in 24 h (limit 3)' }, 30 * H, false, 4 * H);
alerts.find((a) => a.node_id === 'n009')!.acked_by = 'admin';
// a pending queue the mock plays one step every 25 s so the page can be seen live
const script: (() => Alert)[] = [
	() => Object.assign(alerts.find((a) => a.kind === 'reserve_at_risk')!, { active: false, last_ms: Date.now() }),
	() => mk({ kind: 'standby_behind', severity: 'warn', node_id: 'n004', detail: 'pc-04 is 830 changes behind (limit 500)' }, 0),
	() => mk({ kind: 'head_hot', severity: 'warn', node_id: 'n001', detail: 'head CPU 78 % for 60 s (limit 70 %)' }, 0)
];

function breakdown(ctx: Ctx, id: number) {
	const j = ctx.jobs.find((x) => x.id === id);
	if (!j || !j.started_ms) return null;
	const r = rnd(id * 7919);
	const wall = Math.max(5, ((j.finished_ms ?? ctx.now()) - j.started_ms) / 1000);
	const pcs = Math.min(6 + (id % 9), ctx.workers.length); // one row per distinct PC, like the head
	const total = wall * pcs;
	const fail = id % 3 === 0, rec = id % 4 === 1 || fail;
	const frac = { dispatch_wait: 0.03 + r() * 0.03, tail_idle: 0.04 + r() * 0.05, recompute: rec ? 0.05 + r() * 0.04 : 0, failover_pause: fail ? 0.04 + r() * 0.02 : 0 };
	const other = 0.01 + r() * 0.015;
	const compute = total * (1 - Object.values(frac).reduce((a, b) => a + b, 0) - other);
	const per = Array.from({ length: pcs }, (_, i) => {
		const w = ctx.workers[(id + i) % ctx.workers.length]!;
		const slow = i === 2 && id % 2 === 0 ? 0.45 : 1;
		const busy = (compute / pcs) * (0.9 + r() * 0.2) * (slow < 1 ? 0.8 : 1), rate = 5.6e6 * (0.92 + r() * 0.16) * slow;
		return { node_id: w.node_id, host: w.host, chunks: Math.round(busy / 1.4), numbers: Math.round(busy * rate), rate, busy_s: busy };
	});
	return {
		job: id, wall_s: wall,
		pc_seconds: { compute, dispatch_wait: total * frac.dispatch_wait, tail_idle: total * frac.tail_idle, recompute: total * frac.recompute, failover_pause: total * frac.failover_pause, other: total * other },
		per_pc: per, chunk_s: { p50: 1.4, p90: 2.3 + r(), max: 4.1 + 3 * r() },
		stragglers: per.filter((x) => x.rate < 3e6).map((x) => x.node_id), efficiency: compute / total
	};
}

// experiments: 1..64 PCs, 7 repeats, two policies; sim in the shape of docs/sim/e1_scaling.csv
const PCS = [1, 2, 4, 8, 16, 32, 64];
function rate(p: number, serial: number) { return 2.06e7 * p / (1 + serial * (p - 1)); } // Amdahl
function experiment(id: number, name: string, withSim: boolean) {
	const r = rnd(id * 31 + 5), T0 = Date.now() - id * 2.5 * 864e5;
	const csv = ['config,repeat,n,seconds,hashes_per_s,hits,pcs,policy,chunk,virtual'], runs: string[] = [], sim: object[] = [];
	for (let rep = 1; rep <= 7; rep++)
		for (const pol of ['adaptive', 'fixed']) for (const p of PCS) {
			const hs = rate(p, pol === 'adaptive' ? 0.004 : 0.011) * (1 + (r() - 0.5) * (0.01 + 0.002 * p));
			const n = 2e9 * p, sec = n / hs, cfg = `${p} PCs ${pol}`;
			csv.push(`${cfg},${rep},${n},${sec.toFixed(3)},${Math.round(hs)},0,${p},${pol},0,0`);
			runs.push(JSON.stringify({ config: cfg, repeat: rep, job: 100 + runs.length, commit: id === 1 ? 'b1cc499' : '89dc1a8', dirty: false, head: '10.0.0.5:7702', kind: 'synth', n, policy: pol, chunk: 0, virtual: false, seconds: sec, started_ms: T0 + runs.length * 9e4, finished_ms: T0 + runs.length * 9e4 + sec * 1000, wall_start: T0 / 1000, pcs: Array.from({ length: p }, (_, i) => ({ node_id: `n${String(i + 1).padStart(3, '0')}`, host: `pc-${String(i + 1).padStart(2, '0')}`, threads: i % 3 === 0 ? 16 : 8, class: i % 3 === 0 ? 'AMD Ryzen 7 5700G' : 'Intel Core i5-10400' })) }));
            if (withSim && rep <= 3) sim.push({ policy: pol, n_per_pc: 2e9, pcs: p, repeat: rep, throughput_per_s: rate(p, pol === 'adaptive' ? 0.0043 : 0.0102) * (1 + (r() - 0.5) * 0.004), efficiency_pct: 0, makespan_s: sec, head_util_pct: 0.2 * p / 4 });
		}
	return { row: { id, name, submitter: id === 1 ? 'ana' : 'ben', created_ms: T0, configs: PCS.length * 2, repeats: 7, virtual: id === 2, commit: id === 1 ? 'b1cc499' : '89dc1a8', pcs_max: 64 }, raw_csv: csv.join('\n') + '\n', runs_jsonl: runs.join('\n') + '\n', sim: withSim ? sim : null };
}
const experiments = [experiment(1, 'e1-scaling-lab-a', true), experiment(2, 'e1-scaling-virtual', true), experiment(3, 'e2-policy-rerun', false)];

function report(day: string) {
	const r = rnd(hash(day)), d = new Date(day + 'T00:00:00');
	const wk = d.getDay() === 0 || d.getDay() === 6;
	const mean = wk ? 3 + r() * 4 : 14 + r() * 8;
	return {
		day, throughput_peak: (mean + 8 + r() * 4) * 8 * 5.6e6, throughput_mean: mean * 8 * 5.6e6 * 0.8,
		work_done: { numbers: Math.round(mean * 8 * 5.6e6 * 0.8 * 86400 * 0.7), items: Math.round(r() * 4e4), jobs_done: Math.round(mean * 1.6), jobs_failed: Math.floor(r() * 3) },
		pcs: { seen: 30, max_online: Math.round(mean + 6), mean_online: +(mean).toFixed(1) },
		failures: { pcs_lost: Math.floor(r() * 7), recovered_chunks: Math.floor(r() * 90), head_failovers: r() > 0.8 ? 1 : 0 },
		energy_kwh: r() > 0.1 ? +(mean * 2.1 + r() * 10).toFixed(1) : null, busy_share: +(0.45 + r() * 0.4).toFixed(2),
		top_jobs: ['Synth 500G numbers', 'Exam grading (urgent)', 'Hash sweep 1', 'Prime search 2', 'Monte Carlo 6'].map((name, i) => ({ id: 4 + i, name, pc_hours: +((20 - i * 3.5) * (0.8 + r() * 0.4)).toFixed(1) })),
		alerts: Math.floor(r() * 6)
	};
}

let timer: ReturnType<typeof setInterval> | undefined;
export async function research(ctx: Ctx): Promise<Response | null> {
	const { p, m, json, err } = ctx;
	if (!timer) {
		let step = 0;
		timer = setInterval(() => {
			const a = script[step++ % script.length]!();
			for (const s of ctx.subs) s('alert', a);
		}, 25_000);
	}
	let x: RegExpExecArray | null;
	if ((x = /^\/api\/v1\/jobs\/(\d+)\/breakdown$/.exec(p))) {
		const b = breakdown(ctx, Number(x[1]));
		return b ? json({ breakdown: b }) : err(404, 'not_found', `Job ${x[1]} has not started, so it has no breakdown.`);
	}
	if (p === '/api/v1/alerts') return json({ alerts: [...alerts].filter((a) => ctx.url.searchParams.get('active') !== '1' || a.active).sort((a, b) => b.since_ms - a.since_ms) });
	if ((x = /^\/api\/v1\/alerts\/(\d+)\/ack$/.exec(p)) && m === 'POST') {
		if (ctx.RANK[ctx.me.role]! < ctx.RANK.operator!) return err(403, 'forbidden', 'This needs the operator role.');
		const a = alerts.find((y) => y.id === Number(x![1]));
		if (!a) return err(404, 'not_found', 'No such alert.');
		a.acked_by = ctx.me.name;
		for (const s of ctx.subs) s('alert', a);
		return json({ alert: a });
	}
	if (p === '/api/v1/experiments' && m === 'GET') return json({ experiments: experiments.map((e) => e.row).reverse() });
	if (p === '/api/v1/experiments' && m === 'POST') {
		const b = await ctx.body();
		if (ctx.RANK[ctx.me.role]! < ctx.RANK.researcher!) return err(403, 'forbidden', 'This needs the researcher role.');
		if (!b.name || !b.raw_csv) return err(400, 'invalid_request', 'name and raw_csv are required.');
		const e = experiment(experiments.length + 1, String(b.name), false);
		e.row.name = String(b.name);
		experiments.push(e);
		return json({ experiment: e.row });
	}
	if ((x = /^\/api\/v1\/experiments\/(\d+)$/.exec(p))) {
		const e = experiments.find((y) => y.row.id === Number(x![1]));
		return e ? json({ experiment: e.row, raw_csv: e.raw_csv, runs_jsonl: e.runs_jsonl, sim: e.sim }) : err(404, 'not_found', 'No such experiment.');
	}
	if (p === '/api/v1/report') {
		const day = ctx.url.searchParams.get('day') ?? '';
		if (!/^\d{4}-\d{2}-\d{2}$/.test(day)) return err(400, 'invalid_request', 'day must look like 2026-11-05.');
		return json({ report: report(day) });
	}
	return null;
}
