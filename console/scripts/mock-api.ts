// Mock of the ck head HTTP API (docs/API.md) for developing and screenshotting the console.
//   MOCK_AUTH=open (default) | required | bootstrap     bun run scripts/mock-api.ts
// Then: CK_DASH=7942 bun run dev -- --port 5174. Logins in "required" mode: admin / adminadminadmin.
// Fake data only; it serves nothing but /api/v1/* and /stats.
import { research } from './mock-research.ts';
type Role = 'viewer' | 'researcher' | 'operator' | 'admin';
const RANK: Record<Role, number> = { viewer: 0, researcher: 1, operator: 2, admin: 3 };
const PORT = Number(process.env.MOCK_PORT ?? 7942);
const MODE = process.env.MOCK_AUTH ?? 'open';

const users = new Map<string, { password: string; role: Role; created_ms: number; quota?: { max_pcs: number; max_running: number } }>();
if (MODE === 'required') {
	users.set('admin', { password: 'adminadminadmin', role: 'admin', created_ms: Date.now() - 864e5, quota: { max_pcs: 0, max_running: 0 } });
	users.set('vic', { password: 'viewerpass1234', role: 'viewer', created_ms: Date.now() - 3e8 });
	users.set('ana', { password: 'researcherpass1', role: 'researcher', created_ms: Date.now() - 4e8, quota: { max_pcs: 8, max_running: 2 } });
	for (const [i, n] of ['ben', 'chitra', 'dev', 'esha', 'farid', 'gita', 'hari', 'indu', 'jai'].entries()) users.set(n, { password: 'researcherpass1', role: i % 4 === 0 ? 'operator' : i % 4 === 1 ? 'viewer' : 'researcher', created_ms: Date.now() - (5 + i) * 3e7 });
}
const sessions = new Map<string, string>(); // cookie -> user name

type Job = {
	id: number; name: string; kind: 'sha' | 'cmd' | 'synth' | 'list'; from: number; end: number; chunk: number; total: number; done: number;
	state: 'queued' | 'running' | 'done' | 'cancelled' | 'failed'; submitter: string;
	created_ms: number; started_ms: number | null; finished_ms: number | null; error: string | null;
	selector: string; priority: number; policy: string; reducer: string; eta_ms: number | null; items?: string[];
	max_pcs?: number; placement?: string; mem_mb?: number; wait_reason?: 'quota' | 'priority' | 'no_pcs' | null; files?: number;
};
const now = () => Date.now();
const jobs: Job[] = [];
let jobSeq = 0;
const mkJob = (o: Partial<Job> & Pick<Job, 'kind' | 'total'>): Job => {
	const j: Job = {
		id: ++jobSeq, name: '', from: 1, end: 1 + o.total, chunk: 1_000_000, done: 0, state: 'queued', submitter: 'admin',
		created_ms: now(), started_ms: null, finished_ms: null, error: null, selector: '', priority: 2, policy: 'adaptive', reducer: '', eta_ms: null, ...o
	};
	j.end = j.from + j.total;
	jobs.push(j);
	return j;
};
const ago = (ms: number) => Math.round(now() - ms);
mkJob({ name: 'SHA sweep 0-50M', kind: 'sha', total: 50_000_000, done: 50_000_000, state: 'done', created_ms: ago(9e6), started_ms: ago(9e6), finished_ms: ago(8.9e6) });
mkJob({ name: 'Flaky command', kind: 'cmd', total: 8_000_000, done: 3_000_000, state: 'failed', error: 'worker 10.0.0.7 exited with status 2', submitter: 'ana', created_ms: ago(7e6), started_ms: ago(7e6), finished_ms: ago(6.9e6) });
mkJob({ name: 'Old synth', kind: 'synth', total: 1_000_000_000, done: 400_000_000, state: 'cancelled', submitter: 'ana', created_ms: ago(5e6), started_ms: ago(5e6), finished_ms: ago(4.9e6) });
mkJob({ name: 'Synth 500G numbers', kind: 'synth', total: 500_000_000_000, done: 50_000_000_000, state: 'running', created_ms: ago(40_000), started_ms: ago(40_000), selector: '', priority: 2 });
mkJob({ name: 'Exam grading (urgent)', kind: 'sha', total: 80_000_000_000, done: 8_000_000_000, state: 'running', created_ms: ago(30_000), started_ms: ago(30_000), selector: 'pool=night', priority: 0, submitter: 'ana' });
mkJob({ name: 'Hash the word list', kind: 'list', total: 6, done: 6, state: 'done', reducer: 'sort', policy: 'guided', submitter: 'ana', created_ms: ago(3e6), started_ms: ago(3e6), finished_ms: ago(2.9e6), items: ['pear', 'apple', 'fig', 'plum', 'kiwi', 'lime'] });
mkJob({ name: 'Nightly SHA batch', kind: 'sha', total: 200_000_000, submitter: 'ana' });
// ~45 more jobs: a realistic history (mostly done, some failed or cancelled) and a few queued
{
	const who = ['ana', 'ben', 'chitra', 'dev', 'esha', 'admin'], kinds2 = ['sha', 'synth', 'cmd', 'list'] as const;
	const names = ['Hash sweep', 'Prime search', 'Grade batch', 'Render tiles', 'Word list', 'Monte Carlo', 'Checksum audit', 'Nightly SHA', 'Seed scan', 'Matrix probe'];
	for (let i = 0; i < 45; i++) {
		const queued = i >= 42, st = queued ? 'queued' : i % 9 === 4 ? 'failed' : i % 11 === 6 ? 'cancelled' : 'done';
		const total = 1_000_000 * (1 + ((i * 37) % 900)), age = (i + 1) * 5.2e6 * (0.6 + (i % 5) * 0.2);
		mkJob({
			name: `${names[i % names.length]} ${i + 1}`, kind: kinds2[i % 4], total, done: st === 'done' ? total : st === 'queued' ? 0 : Math.floor(total * 0.4), state: st,
			submitter: who[(i * 7) % who.length], priority: i % 13 === 0 ? 0 : i % 5 === 1 ? 1 : i % 7 === 3 ? 3 : i % 17 === 5 ? 4 : 2,
			selector: i % 3 === 0 ? 'lab="Lab A"' : i % 8 === 2 ? 'pool=night' : '', error: st === 'failed' ? `worker 10.0.0.${10 + (i % 20)} exited with status ${1 + (i % 3)}` : null,
			created_ms: ago(age), started_ms: queued ? null : ago(age - 2e4), finished_ms: st === 'done' || st === 'failed' || st === 'cancelled' ? ago(age - 4e5) : null
		});
	}
}

const workers = Array.from({ length: 30 }, (_, i) => {
	const threads = i % 3 === 0 ? 16 : 8;
	return {
		id: i + 1, addr: `10.0.0.${10 + i}:7701`, threads, chunks: 0, numbers: 0, secs: 0, rate: 0, busy: false,
		cpu: 0, mem_used: 0, mem_total: (i % 2 ? 16 : 32) * 2 ** 30, version: 1, node_id: `n${String(i + 1).padStart(3, '0')}`,
		host: `pc-${String(i + 1).padStart(2, '0')}`, virtual: i >= 28, presence: 'idle' as 'busy' | 'idle', blocked: false, cap: null as number | null,
		inv: { cpu_model: i % 3 === 0 ? 'AMD Ryzen 7 5700G' : 'Intel Core i5-10400', cores_physical: threads / 2, cores_logical: threads,
			ram_bytes: (i % 2 ? 16 : 32) * 2 ** 30, os: 'Ubuntu 24.04', kernel: '6.8.0', arch: 'x86_64', ck_version: '0.2.0' }
	};
});

// ---- fleet: node registry, labs, join codes
type NState = 'pending' | 'active' | 'cordoned' | 'draining' | 'maintenance' | 'retired';
type Node = { node_id: string; host: string; addr: string; inventory: unknown; first_seen_ms: number; last_seen_ms: number; state: NState; state_reason: string; state_by: string; lab_id: number | null; x: number | null; y: number | null; online: boolean; calib_mhs: number | null; presence: 'busy' | 'idle'; labels: Record<string, string>; pools: string[]; class: string; power: 'awake' | 'asleep' | 'waking'; power_since_ms?: number; identify_until_ms?: number };
const labs: { id: number; name: string; w: number; h: number; created_ms: number }[] = [
	{ id: 1, name: 'Lab A', w: 6, h: 4, created_ms: ago(8e8) },
	{ id: 2, name: 'Lab B', w: 5, h: 3, created_ms: ago(7e8) }
];
let labSeq = 2;
const nodes = new Map<string, Node>();
workers.forEach((w, i) => {
	const lab = i < 20 ? labs[0] : i < 26 ? labs[1] : null;
	const k = lab ? (lab === labs[0] ? i : i - 20) : 0;
	nodes.set(w.node_id!, {
		node_id: w.node_id!, host: w.host!, addr: w.addr, inventory: w.inv, first_seen_ms: ago(5e8), last_seen_ms: now(),
		state: i >= 26 && i < 29 ? 'pending' : 'active', state_reason: '', state_by: '',
		lab_id: lab?.id ?? null, x: lab ? k % lab.w : null, y: lab ? Math.floor(k / lab.w) : null, online: true,
		calib_mhs: 40 + (i % 5) * 6, presence: 'idle' as 'busy' | 'idle', labels: i % 4 === 0 ? { room: String(1 + (i % 3)), gpu: 'yes' } : i === 7 ? { broken: '' } : {}, pools: [], power: 'awake', class: `${(w.inv as any).cpu_model}/${w.threads}c/${(w.inv as any).ram_bytes / 2 ** 30}G`
	});
});
// ---- organisation: pools, caps, reserves, a small selector subset
const pools = new Map<string, { name: string; members: string[]; note: string }>();
pools.set('night', { name: 'night', members: ['n001', 'n002', 'n003', 'n004', 'n021', 'n022'], note: 'Overnight batch machines' });
pools.set('gpu', { name: 'gpu', members: ['n001', 'n005', 'n009'], note: '' });
const syncPools = () => { for (const n of nodes.values()) n.pools = [...pools.values()].filter((p) => p.members.includes(n.node_id)).map((p) => p.name); };
syncPools();
const caps: { id: number; selector: string; max_threads: number | null; note: string }[] = [{ id: 1, selector: 'lab="Lab B"', max_threads: 4, note: 'Students work here by day' }];
// wave 15: output files on finished cmd jobs, and why queued jobs wait
for (const j of jobs) {
	if (j.state === 'done' && (j.kind === 'cmd' || j.id === 1)) j.files = 2 + (j.id % 3);
	if (j.state === 'queued') j.wait_reason = j.id % 3 === 0 ? 'quota' : j.id % 3 === 1 ? 'priority' : 'no_pcs';
}
const fileList = (j: Job) => Array.from({ length: j.files ?? 0 }, (_, i) => ({ chunk: [j.from + i * j.chunk, j.from + (i + 1) * j.chunk], name: i % 2 ? `scores-${i}.csv` : `out-${i}.json`, sha: Bun.SHA256.hash(`${j.id}-${i}`, 'hex'), size: 2_048 * (i + 1) * 37 }));
const templates: { name: string; spec: Record<string, unknown>; note: string; owner: string; created_ms: number }[] = [
	{ name: 'nightly-sha', spec: { kind: 'sha', n: 200_000_000, sha: 'ab12cd34', priority: 'low', selector: 'pool=night' }, note: 'Nightly SHA batch on the night pool', owner: 'ana', created_ms: Date.now() - 6e8 },
	{ name: 'grade-exams', spec: { kind: 'synth', n: 5_000_000, synth_ns: 800, priority: 'urgent' }, note: 'Exam grading, urgent', owner: 'ben', created_ms: Date.now() - 2e8 }
];
// ---- network (UI-07): shape of rust api.rs net_json
let uplinkProbed = false;
const netNodes = () => [...nodes.values()].filter((n) => n.online).map((n, i) => ({
	node_id: n.node_id, host: n.host, lab: labs.find((l) => l.id === n.lab_id)?.name ?? 'Unplaced',
	net_mbps: i % 11 === 7 ? null : (i % 5 === 0 ? 94 : 940) - (i % 7) * 4, rtt_ms: i % 11 === 7 ? null : 0.4 + (i % 6) * 0.35, probed_ms: i % 11 === 7 ? null : now() - (i % 9) * 60_000 - 5_000
}));
const netJson = () => {
	const nn = netNodes(), by = new Map<string, number[]>();
	for (const n of nn) if (n.net_mbps != null) (by.get(n.lab) ?? by.set(n.lab, []).get(n.lab)!).push(n.net_mbps);
	const labsOut = [...by].map(([lab, v]) => { v.sort((a, b) => a - b); return { lab, pcs: v.length, mbps: v[v.length >> 1] }; });
	return { head_uplink_mbps: uplinkProbed ? 940 : 1000, head_uplink_assumed: !uplinkProbed, head_bytes_sent: 18_400_000_000, nodes: nn, labs: labsOut };
};
const reserves: { id: number; selector: string; keep_n: number; keep_pct: number; note: string }[] = [{ id: 1, selector: 'lab="Lab A"', keep_n: 3, keep_pct: 0, note: 'Free for students' }];
let capSeq = 1, resSeq = 1;
// ---- time rules (mock clock = local time of this process)
type Win = { id: number; selector: string; days: string; start: string; end: string; action: 'block' | 'cap'; cap_threads: number | null; note: string };
type Resv = { id: number; selector: string; start_ms: number; end_ms: number; note: string };
const DAYN = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];
const hhmm = (d: Date) => `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
const t0 = new Date();
const wins: Win[] = [
	{ id: 1, selector: 'lab="Lab B"', days: 'mon-fri', start: '09:00', end: '17:00', action: 'cap', cap_threads: 2, note: 'Classes in Lab B' },
	{ id: 2, selector: 'pool=gpu', days: DAYN[(t0.getDay() + 6) % 7], start: hhmm(new Date(t0.getTime() - 3_600_000)), end: hhmm(new Date(t0.getTime() + 3_600_000)), action: 'block', cap_threads: null, note: 'GPU demo (active now)' },
	{ id: 3, selector: '', days: 'fri,sat', start: '22:00', end: '06:00', action: 'block', cap_threads: null, note: 'Building closed overnight' }
];
const day0 = new Date(t0.getFullYear(), t0.getMonth(), t0.getDate() + 1, 9).getTime();
const resvs: Resv[] = [{ id: 1, selector: 'lab="Lab A"', start_ms: day0, end_ms: day0 + 3 * 3_600_000, note: 'Midterm exam' }];
let winSeq = 3, rsvSeq = 1;
const toMin = (s: string) => { const [h, m] = s.split(':').map(Number); return h * 60 + m; };
const winActive = (w: Win, d = new Date()) => {
	const di = (d.getDay() + 6) % 7, m = d.getHours() * 60 + d.getMinutes(), a = toMin(w.start), b = toMin(w.end);
	const on = (i: number) => w.days.split(',').some((t) => { const [x, y] = t.split('-').map((n) => DAYN.indexOf(n)); const z = t.includes('-') ? y : x; return x <= z ? i >= x && i <= z : i >= x || i <= z; });
	return a < b ? on(di) && m >= a && m < b : (on(di) && m >= a) || (on((di + 6) % 7) && m < b);
};
workers[2].presence = 'busy'; // pc-03 is in use by a person
const winRow = (w: Win) => ({ ...w, active_now: winActive(w) });
const resvRow = (r: Resv) => ({ ...r, active_now: r.start_ms <= now() && now() < r.end_ms });
// pc-03 is in use by a person; time rules decide blocked / cap per worker on every tick
const heldFor = (r: { selector: string; keep_n: number; keep_pct: number }) => {
	const ids = matchIds(r.selector).sort();
	return ids.slice(0, Math.max(r.keep_n, Math.ceil((ids.length * r.keep_pct) / 100)));
};
// Subset: space-separated terms of key=a,b | key!=v | key>=N | key | !key, trailing * prefix. Keys: host lab state pool cores threads ram_gb class + labels.
function attrOf(n: Node, key: string): string | string[] | undefined {
	const inv = (n.inventory ?? {}) as any;
	if (key === 'host' || key === 'node') return key === 'host' ? n.host : n.node_id;
	if (key === 'lab') return labs.find((l) => l.id === n.lab_id)?.name;
	if (key === 'state') return n.state;
	if (key === 'online') return String(n.online);
	if (key === 'pool') return n.pools;
	if (key === 'cores' || key === 'threads') return String(inv.cores_logical);
	if (key === 'ram_gb') return String(Math.round(inv.ram_bytes / 2 ** 30));
	if (key === 'class') return n.class;
	return n.labels[key];
}
function parseSel(q: string): ((n: Node) => boolean) | string {
	const terms = q.match(/(?:[^\s"]|"[^"]*")+/g) ?? [];
	const fs: ((n: Node) => boolean)[] = [];
	for (const t of terms) {
		const m = /^(!?)([a-z_][a-z0-9_.-]*)(?:(!=|>=|<=|=|>|<)(.+))?$/i.exec(t);
		if (!m || (m[1] && m[3]) || (!m[3] && !m[2])) return `bad term \`${t}\``;
		const [, neg, key, op, raw] = m;
		const vals = (raw ?? '').replace(/^"|"$/g, '').split(',');
		fs.push((n) => {
			const a = attrOf(n, key);
			const list = Array.isArray(a) ? a : a == null ? [] : [a];
			if (!op) { const has = list.some((x) => x !== '' && x !== 'false' && x !== '0'); return neg ? !has : has; }
			const hit = (v: string) => list.some((x) => (v.endsWith('*') ? x.startsWith(v.slice(0, -1)) : x === v));
			if (op === '=') return vals.some(hit);
			if (op === '!=') return !vals.some(hit);
			const x = Number(list[0]), v = Number(vals[0]);
			return op === '>=' ? x >= v : op === '<=' ? x <= v : op === '>' ? x > v : x < v;
		});
	}
	return (n) => fs.every((f) => f(n));
}
function matchIds(q: string): string[] {
	const f = parseSel(q);
	return typeof f === 'string' ? [] : [...nodes.values()].filter(f).map((n) => n.node_id);
}
const sendAll = (name: string, data: unknown) => { for (const s of subs) s(name, data); };
const changed = (what: string) => sendAll('changed', { what });
// contract: the node's `power` is {state, since_ms} on the wire
const wire = (n: Node) => ({ ...n, power: { state: n.power, since_ms: n.power_since_ms ?? null }, power_since_ms: undefined, identify_until_ms: (n.identify_until_ms ?? 0) > now() ? n.identify_until_ms : null });
const setNode = (id: string, patch: Partial<Node>) => { const n = nodes.get(id)!; if (patch.power && patch.power !== n.power) patch = { ...patch, power_since_ms: now() }; Object.assign(n, patch); sendAll('node', wire(n)); };
const takes = (id: string) => { const n = nodes.get(id)!; return n.online && (n.state === 'active' || n.state === 'draining'); };
// states change over time: pc-05 drains then cordons, pc-30 (unplaced) drops off, pc-12 comes back from maintenance
setTimeout(() => setNode('n005', { state: 'draining', state_reason: 'disk check', state_by: 'admin' }), 12_000);
setTimeout(() => setNode('n005', { state: 'cordoned' }), 28_000);
setTimeout(() => { setNode('n030', { online: false }); logEvent('node.lost', '', 'pc-30 lost'); }, 20_000);
Object.assign(nodes.get('n004')!, { online: false, power: 'asleep' }); // an asleep PC from the start
[...nodes.values()].forEach((n, i) => (n.power_since_ms = now() - (2 + (i % 9)) * 600_000 - i * 37_000));
Object.assign(nodes.get('n012')!, { state: 'maintenance', state_reason: 'RAM swap', state_by: 'admin' });

type Ev = { id: number; ts_ms: number; kind: string; actor: string; detail: string };
const events: Ev[] = [];
let evSeq = 0;
const subs = new Set<(name: string, data: unknown) => void>();
// ---- structured log lines (docs/LOGGING.md), JSON per line, newest last in memory
type LogLine = Record<string, unknown> & { ts_ms: number; level: 'error' | 'warn' | 'info' | 'debug'; role: string; node: string; kind: string };
const logLines: LogLine[] = [];
const LV = { error: 0, warn: 1, info: 2, debug: 3 } as const;
let logLive = false; // after the seeded history: new lines also go out as SSE `log`, batched every 500 ms, at most 200 per batch
let logBatch: LogLine[] = [];
const pushLog = (l: LogLine) => { logLines.push(l); if (logLines.length > 40_000) logLines.splice(0, 5_000); if (logLive) logBatch.push(l); };
setInterval(() => { if (logBatch.length) { sendAll('log', { lines: logBatch.slice(-200) }); logBatch = []; } }, 500);
setTimeout(() => (logLive = true), 100);

const EV_KINDS: [string, string, (n: number) => string][] = [
	['node.join', '', (n) => `pc-${String(1 + (n % 30)).padStart(2, '0')} joined`],
	['node.lost', '', (n) => `pc-${String(1 + ((n * 7) % 30)).padStart(2, '0')} lost`],
	['job.submit', 'ana', (n) => `job ${1 + (n % 50)} submitted`],
	['job.start', '', (n) => `job ${1 + (n % 50)} started`],
	['job.done', '', (n) => `job ${1 + (n % 50)} finished`],
	['job.cancel', 'admin', (n) => `job ${1 + (n % 50)} cancelled`],
	['auth.login', 'admin', () => 'login from 127.0.0.1'],
	['power.sleep', '', (n) => `${1 + (n % 6)} PCs went to sleep`],
	['power.wake', '', (n) => `pc-${String(1 + (n % 30)).padStart(2, '0')} woke up`],
	['tune.changed', 'admin', () => 'lost_after_ms 15000 -> 20000'],
	['ha.recruit', '', (n) => `pc-${String(1 + (n % 30)).padStart(2, '0')} became a standby`],
	['node.approve', 'admin', (n) => `pc-${String(1 + (n % 30)).padStart(2, '0')} -> active`],
	['pool.change', 'ben', () => 'PUT night'],
	['user.create', 'admin', () => 'created esha as researcher']
];
const ACTORS = ['ana', 'ben', 'chitra', 'dev', 'admin'];
const logEvent = (kind: string, actor: string, detail: string, ts = now()) => {
	const e = { id: ++evSeq, ts_ms: ts, kind, actor, detail };
	events.push(e);
	pushLog({ ts_ms: ts, level: kind.endsWith('lost') ? 'warn' : 'info', role: 'head', node: 'n001', kind, actor, msg: detail });
	for (const s of subs) s('event', e);
};
// 2,000 events: dense in the last hour, thinning over ~2.5 days (oldest first so ids rise with time)
for (let i = 1999; i >= 0; i--) {
	const [k, a0, d] = EV_KINDS[(i * 7 + (i >> 3)) % EV_KINDS.length];
	logEvent(k, a0 && i % 3 === 0 ? ACTORS[i % ACTORS.length] : a0, d(i), ago((i / 2000) ** 2.5 * 2.5 * 864e5 + i * 1500));
}
// ~12,000 log lines over the last 24 h: api requests, chunks, node and ha traffic
{
	const KL: [string, LogLine['level'], string, string][] = [
		['api.request', 'debug', 'head', 'GET /api/v1/cluster'], ['api.request', 'info', 'head', 'POST /api/v1/jobs'], ['api.request', 'warn', 'head', 'GET /api/v1/jobs/999'],
		['chunk.requeue', 'warn', 'head', 'chunk requeued after a slow PC'], ['chunk.dispatch', 'debug', 'head', 'chunk dispatched'], ['chunk.end', 'debug', 'agent', 'chunk finished'],
		['node.connect', 'info', 'head', 'agent connected'], ['node.disconnect', 'warn', 'head', 'agent disconnected'], ['ha.lag', 'info', 'head', 'standby lag class changed'],
		['standby.follow', 'info', 'standby', 'following the head'], ['power.eco', 'info', 'head', 'eco: 3 PCs to sleep'], ['agent.calibration', 'info', 'agent', 'calibrated 42.1 MH/s'],
		['job.started', 'info', 'head', 'job started'], ['job.complete', 'info', 'head', 'job complete'], ['tune.sent', 'info', 'head', 'tunables sent to 28 PCs'], ['node.denied', 'error', 'head', 'join code expired']
	];
	for (let i = 0; i < 12_000; i++) {
		const [kind, level, role, msg] = KL[(i * 5 + (i >> 4)) % KL.length];
		const ts = ago((i / 12_000) ** 1.6 * 864e5 + i * 40);
		const node = role === 'head' ? 'n001' : `n${String(1 + (i % 30)).padStart(3, '0')}`;
		pushLog({ ts_ms: ts, level, role, node, kind, msg: `${msg} #${i}`, ...(kind === 'api.request' ? { method: msg.split(' ')[0], path: msg.split(' ')[1], status: level === 'warn' ? 404 : 200, ms: 1 + (i % 17), user: ACTORS[i % ACTORS.length] } : { job: 1 + (i % 50), a: i * 1000, b: i * 1000 + 999 }) });
	}
	logLines.sort((x, y) => x.ts_ms - y.ts_ms);
}
setInterval(() => {
	for (let i = 0; i < 3; i++) pushLog({ ts_ms: now(), level: i === 2 && Math.random() < 0.2 ? 'warn' : i === 0 ? 'debug' : 'info', role: 'head', node: 'n001', kind: i === 0 ? 'api.request' : i === 1 ? 'chunk.end' : 'ha.lag', msg: i === 0 ? 'GET /api/v1/cluster' : i === 1 ? 'chunk finished' : 'standby lag class changed', ...(i === 0 ? { method: 'GET', path: '/api/v1/cluster', status: 200, ms: 2, user: 'admin' } : {}) });
}, 1500);

const runningAll = () => jobs.filter((j) => j.state === 'running');
const running = () => runningAll()[0] ?? null;
const BASE = 30;
function refreshRules() {
	for (const w of workers.slice(0, BASE)) {
		const n = nodes.get(w.node_id!)!;
		n.presence = w.presence;
		const hit = wins.filter((x) => winActive(x) && matchIds(x.selector).includes(w.node_id!));
		const rsv = resvs.some((r) => resvRow(r).active_now && matchIds(r.selector).includes(w.node_id!));
		w.blocked = rsv || hit.some((x) => x.action === 'block');
		const caps = hit.filter((x) => x.action === 'cap').map((x) => x.cap_threads!);
		w.cap = caps.length ? Math.min(...caps) : null;
	}
}
function tick() {
	refreshRules();
	const j = running();
	const others = runningAll().slice(1);
	for (const o of others) { o.done = Math.min(workers.length > BASE ? o.total * 0.5 : o.total, o.done + 20_000_000 * 0.5 * 30); for (const s of subs) s('job', o); }
	let busy = 0;
	for (const w of workers) {
		w.busy = !!j && takes(w.node_id!) && w.id % 7 !== 0 && !w.blocked && w.presence !== 'busy';
		if (w.busy) busy++;
	}
	// ~50 finished chunks/s across the busy base PCs (1.5/s per PC in the 1000-PC probe), jittered
	const perTick = busy ? (workers.length > BASE ? 0.75 * workers.length / busy : 25 / busy) : 0;
	for (const w of workers) {
		w.rate = w.busy ? w.threads * (4.5e6 + Math.random() * 1.5e6) : 0;
		w.cpu = w.busy ? 90 + Math.random() * 9 : 2 + Math.random() * 5;
		w.mem_used = w.mem_total * (0.3 + Math.random() * 0.1);
		if (w.busy) { const e = perTick * (0.6 + Math.random() * 0.8); w.numbers += w.rate * 0.5; w.chunks += Math.floor(e) + (Math.random() < e % 1 ? 1 : 0); w.secs += 0.5; }
	}
	if (j) {
		j.done = Math.min(workers.length > BASE ? j.total * 0.5 : j.total, Math.floor(j.done + workers.reduce((a, w) => a + w.rate, 0) * 0.5)); // the 1000-PC probe keeps its jobs running
		if (j.done >= j.total) {
			j.state = 'done'; j.finished_ms = now();
			logEvent('job.done', '', `job ${j.id} finished`);
			const q = jobs.find((x) => x.state === 'queued');
			if (q) { q.state = 'running'; q.started_ms = now(); logEvent('job.start', '', `job ${q.id} started`); }
		}
		j.eta_ms = j.done > 0 ? now() + ((j.total - j.done) / Math.max(1, workers.reduce((a, w) => a + w.rate, 0))) * 1000 : null;
		for (const s of subs) s('job', j);
	}
	void busy;
}
setInterval(tick, 500);
const cluster = () => ({ now_ms: now(), workers: workers.filter((w) => nodes.get(w.node_id!)!.online), job: running(), jobs: runningAll() });

// HA: head + 2 standbys (target 2). After 30 s the head "fails": candidate for 8 s, then the first standby is primary with epoch+1.
// One standby is then missing for 6 s until a recruited PC joins (ha.recruit event, lag 5 then in sync 2 s later).
const haT0 = now();
let haSeq = 1200;
let powerCfg = { mode: 'eco', idle_min: 15, keep_awake: 4, wake_on_demand: true };
let haCfg = { target_standbys: 2, selector: '!virtual' };
const ha = { head: 'n001', role: 'primary' as 'primary' | 'standby' | 'candidate', epoch: 3, standbys: [{ node_id: 'n018', lag: 0 }, { node_id: 'n022', lag: 0 }] };
const hostOf = (id: string) => nodes.get(id)?.host ?? id;
const pushHa = () => sendAll('ha', haState());
const headSince = { ms: now() - 3 * 3_600_000 - 14 * 60_000 };
const haState = () => {
	const connected = workers.filter((w) => nodes.get(w.node_id!)!.online).length;
	const wob = (id: string) => (id === 'n022' && Math.floor(now() / 1000) % 5 === 0 ? 2 : 0); // a little lag now and then
	const sb = ha.role === 'candidate' ? [] : ha.standbys.map((s) => ({ node_id: s.node_id, lag: Math.max(s.lag, wob(s.node_id)) }));
	return {
		role: ha.role, epoch: ha.epoch, last_seq: haSeq, durable_seq: haSeq - Math.max(0, ...sb.map((s) => s.lag)),
		standbys: sb.map((s) => ({ ...s, acked_seq: haSeq - s.lag })), membership: nodes.size, connected, dispatching: ha.role === 'primary',
		head: ha.role === 'candidate' ? null : { node_id: ha.head, addr: nodes.get(ha.head)?.addr ?? '', since_ms: headSince.ms }
	};
};
function recruit() {
	const taken = new Set(ha.standbys.map((s) => s.node_id));
	const id = matchIds(haCfg.selector).find((i) => nodes.get(i)!.online && nodes.get(i)!.power === 'awake' && !taken.has(i) && i !== ha.head);
	if (!id || ha.standbys.length >= haCfg.target_standbys) return;
	ha.standbys.push({ node_id: id, lag: 5 });
	logEvent('ha.recruit', '', `${hostOf(id)} became a standby`);
	pushHa();
	setTimeout(() => { const s = ha.standbys.find((x) => x.node_id === id); if (s) { s.lag = 0; pushHa(); } }, 2_000);
}
/** The head goes away `after` ms from now: candidate 8 s, then standbys[0] (or `first`) is head; a recruit follows 6 s later. */
function failover(after: number, first?: string) {
	setTimeout(() => {
		const at = ha.standbys.findIndex((x) => x.node_id === first);
		if (at > 0) ha.standbys.unshift(...ha.standbys.splice(at, 1));
		ha.role = 'candidate';
		pushHa();
		setTimeout(() => {
			const head = ha.standbys.shift();
			if (head) ha.head = head.node_id;
			ha.role = 'primary';
			ha.epoch++;
			headSince.ms = now();
			logEvent('ha.promote', '', `${head ? hostOf(head.node_id) : 'a standby'} is the head, term ${ha.epoch}`);
			pushHa();
			setTimeout(recruit, 6_000);
		}, 8_000);
	}, after);
}
failover(Number(process.env.MOCK_FAILOVER_MS ?? 30_000));
setInterval(() => { haSeq += 3; }, 1000);
let lastHa = '';
setInterval(() => {
	const h = haState();
	const k = JSON.stringify([h.role, h.epoch, h.standbys.map((x) => x.lag)]);
	if (k !== lastHa) { lastHa = k; pushHa(); }
}, 500);

// power: a few PCs asleep and one stuck waking from the start; later a bulk sleep and a wake, as eco mode would do
for (const id of ['n006', 'n007', 'n008', 'n015']) Object.assign(nodes.get(id)!, { online: false, power: 'asleep' });
Object.assign(nodes.get('n010')!, { online: false, power: 'waking' });
const hm = () => hhmm(new Date());
setTimeout(() => { for (const id of ['n016', 'n017', 'n019']) setNode(id, { online: false, power: 'asleep' }); logEvent('power.sleep', '', `3 PCs went to sleep, ${hm()}`); }, 15_000);
setTimeout(() => { setNode('n006', { power: 'waking' }); setTimeout(() => { setNode('n006', { online: true, power: 'awake' }); logEvent('power.wake', '', `pc-06 woke up, ${hm()}`); }, 6_000); }, 22_000);

// PCs that drop off and come back (pc-30 stays gone for a while), and a late wake
const lose = (id: string, back: number) => {
	const n = nodes.get(id);
	if (!n || !n.online || n.power !== 'awake' || n.state === 'maintenance') return;
	setNode(id, { online: false });
	logEvent('node.lost', '', `${n.host} lost`);
	setTimeout(() => { setNode(id, { online: true }); logEvent('node.join', '', `${n.host} joined`); }, back);
};
const FLAPPY = ['n020', 'n021', 'n023', 'n024', 'n025', 'n026', 'n009', 'n011'];
let flap = 0;
setInterval(() => lose(FLAPPY[flap++ % FLAPPY.length], 7_000 + Math.random() * 8_000), 11_000);
setTimeout(() => { setNode('n030', { online: true }); logEvent('node.join', '', 'pc-30 joined'); }, 52_000);
setTimeout(() => { setNode('n016', { power: 'waking' }); setTimeout(() => { setNode('n016', { online: true, power: 'awake' }); logEvent('power.wake', '', `pc-16 woke up, ${hm()}`); }, 5_000); }, 40_000);

// ---- bigger fleets for the canvas FPS probe: any request with ?pcs=N (or MOCK_PCS=N) resizes the fleet; ?pcs=0 goes back to 30
function setFleet(n: number) {
	const want = Math.max(0, Math.min(5000, Math.floor(n) || 0));
	if (want === workers.length - BASE) return;
	for (const w of workers.splice(BASE)) nodes.delete(w.node_id!);
	labs.splice(0, labs.length, ...labs.filter((l) => l.id <= 100));
	for (let i = 0; i < want; i++) {
		const li = Math.floor(i / 100);
		if (i % 100 === 0) labs.push({ id: 101 + li, name: want <= 400 ? `Lab ${'CDEFGH'[li] ?? `S${li + 1}`}` : `Sim ${li + 1}`, w: 10, h: 10, created_ms: ago(1e8) });
		const id = `x${String(i + 1).padStart(4, '0')}`, threads = i % 3 === 0 ? 16 : 8;
		const w = { ...workers[0], id: 1000 + i, addr: `10.1.${Math.floor(i / 250)}.${i % 250}:7701`, threads, chunks: 0, numbers: 0, busy: false, rate: 0, node_id: id, host: `sim-${String(i + 1).padStart(4, '0')}`, virtual: true, presence: 'idle' as const, blocked: false, cap: null };
		workers.push(w);
		const inv = { ...(w.inv as any), cpu_model: ['AMD Ryzen 7 5700G', 'Intel Core i5-10400', 'Intel Core i7-12700'][i % 3], cores_logical: threads, ram_bytes: [8, 16, 32][i % 3] * 2 ** 30 };
		w.inv = inv;
		// a believable mix for the lists (only for the 200-PC default; the 1,000-PC probe stays all-active)
		const mix = want <= 400 ? (i % 53 === 7 ? 'pending' : i % 41 === 3 ? 'maintenance' : i % 37 === 5 ? 'cordoned' : i % 61 === 9 ? 'retired' : 'active') : 'active';
		const asleep = want <= 400 && i % 23 === 4, lost = want <= 400 && i % 43 === 11;
		nodes.set(id, { ...nodes.get('n001')!, node_id: id, host: w.host!, addr: w.addr, inventory: inv, lab_id: 101 + li, x: (i % 100) % 10, y: Math.floor((i % 100) / 10), state: mix as NState, online: !asleep && !lost, power: asleep ? 'asleep' : 'awake', presence: 'idle', labels: want <= 400 && i % 6 === 0 ? { room: String(1 + (i % 4)), gpu: i % 12 === 0 ? 'yes' : 'no' } : {}, pools: [] });
	}
}
setFleet(Number(process.env.MOCK_PCS ?? 170)); // 30 hand-made PCs + 170 = 200 (MOCK_PCS=0 for the original 30)
setInterval(() => {
	if (workers.length <= BASE) return;
	const w = workers[BASE + Math.floor(Math.random() * (workers.length - BASE))];
	if (w.id % 5 === 0) lose(w.node_id!, 4_000);
}, 1_500);

// ---- tunables: copy of rust/src/tune.rs KNOBS [key, group, default, min, max, unit, scope, help]
const KNOBS: [string, string, number, number, number, string, 'head' | 'agent' | 'both', string][] = [
	["heartbeat_s", "Liveness", 1.0, 0.2, 10.0, "s", "both", "How often head and agents send a heartbeat."],
	["lost_after_s", "Liveness", 5.0, 1.0, 120.0, "s", "both", "Silence after which a PC (or the head) counts as lost and its work moves elsewhere."],
	["pipeline", "Dispatch", 2.0, 1.0, 16.0, "chunks", "head", "Chunks each PC holds at once: one computing, the rest queued."],
	["target_chunk_s", "Dispatch", 1.0, 0.05, 60.0, "s", "head", "Adaptive chunks aim to take this long on the PC that gets them."],
	["max_chunk", "Dispatch", 50000000.0, 1000.0, 1000000000000.0, "numbers", "head", "Largest chunk the head ever hands out."],
	["max_attempts", "Dispatch", 3.0, 1.0, 20.0, "tries", "head", "Tries per range before a job fails (failed program runs)."],
	["max_events", "Dispatch", 2000.0, 100.0, 100000.0, "chunks", "head", "Finished chunks kept for the live task stream."],
	["member_window_s", "Head failover", 60.0, 5.0, 3600.0, "s", "head", "PCs seen within this window count toward the majority a new head waits for; ones that never return drop out this long after a takeover."],
	["noq_grace_s", "Head failover", 10.0, 2.0, 600.0, "s", "head", "A head with standbys that has no majority for this long stops handing out work (split-brain guard)."],
	["standby_step_s", "Head failover", 2.0, 0.5, 60.0, "s", "head", "Extra wait per place in the standby order, so standbys take over one at a time."],
	["ring_max", "Head failover", 100000.0, 1000.0, 10000000.0, "changes", "head", "Changes kept in memory for standbys that fall behind; older gaps need a full copy."],
	["switchover_wait_s", "Head failover", 5.0, 1.0, 120.0, "s", "head", "Planned switchover: how long to wait for the standby to confirm."],
	["client_failover_s", "Head failover", 60.0, 5.0, 3600.0, "s", "agent", "How long `ck run` keeps looking for a new head after losing one."],
	["target_standbys", "Dynamic heads", 2.0, 0.0, 5.0, "standbys", "head", "Standby heads to keep; a missing one is recruited automatically (also in Standby heads)."],
	["recruit_tick_s", "Dynamic heads", 10.0, 1.0, 600.0, "s", "head", "How often the head checks whether a standby is missing."],
	["recruit_every_s", "Dynamic heads", 60.0, 5.0, 3600.0, "s", "head", "At most one new standby request per this interval."],
	["recruit_retry_s", "Dynamic heads", 600.0, 30.0, 86400.0, "s", "head", "A PC that was asked (or failed) is not asked again for this long."],
	["head_w_speed", "Dynamic heads", 1.0, 0.0, 10.0, "weight", "head", "Head choice: weight of the PC's measured speed (calibration)."],
	["head_w_memory", "Dynamic heads", 1.0, 0.0, 10.0, "weight", "head", "Head choice: weight of RAM and cores."],
	["head_w_stability", "Dynamic heads", 2.0, 0.0, 10.0, "weight", "head", "Head choice: weight of staying connected (few losses in the last day)."],
	["head_w_free", "Dynamic heads", 1.0, 0.0, 10.0, "weight", "head", "Head choice: weight of not being used by a person right now."],
	["eco_tick_s", "Power", 30.0, 5.0, 3600.0, "s", "head", "How often eco mode looks for PCs to sleep or wake."],
	["waking_for_s", "Power", 120.0, 10.0, 3600.0, "s", "head", "A PC that does not reconnect this long after a wake packet shows 'did not wake'."],
	["wake_retry_s", "Power", 60.0, 5.0, 3600.0, "s", "head", "Wait before sending another wake packet to the same PC."],
	["wake_max", "Power", 64.0, 1.0, 10000.0, "PCs", "head", "Most PCs woken at once for waiting work."],
	["energy_every_s", "Power", 10.0, 1.0, 600.0, "s", "agent", "How often agents report CPU energy (RAPL)."],
	["presence_idle_s", "Presence", 300.0, 10.0, 86400.0, "s", "agent", "A PC counts as free again after nobody touched it for this long."],
	["presence_probe_s", "Presence", 2.0, 0.5, 60.0, "s", "agent", "How often agents check keyboard and mouse activity."],
	["calib_s", "Presence", 3.0, 0.5, 60.0, "s", "agent", "Length of the speed test an agent runs when it joins."],
	["unacked_max", "Agent limits", 10000.0, 100.0, 10000000.0, "results", "agent", "Results an agent keeps until the head confirms them."],
	["unacked_max_mb", "Agent limits", 256.0, 1.0, 65536.0, "MB", "agent", "Memory for unconfirmed results; the oldest go first."],
	["out_max_mb", "Agent limits", 64.0, 1.0, 4096.0, "MB", "agent", "Program output kept per chunk."],
	["push_max_kb", "Agent limits", 64.0, 1.0, 65536.0, "KB", "head", "Largest script `ck run --push` may send."],
	["max_body_mb", "API", 1.0, 0.01, 1024.0, "MB", "head", "Largest API request body."],
	["max_list_body_mb", "API", 64.0, 1.0, 4096.0, "MB", "head", "Largest list-job upload (signed-in operators only)."],
	["max_items", "API", 1000000.0, 1.0, 100000000.0, "items", "head", "Most items in one list job."],
	["max_item_bytes", "API", 4096.0, 16.0, 1048576.0, "bytes", "head", "Longest single item in a list job."],
	["session_idle_h", "Security", 12.0, 0.1, 720.0, "h", "head", "Sign-out after this long without activity."],
	["session_max_d", "Security", 7.0, 0.1, 365.0, "days", "head", "Sign-out after this long in any case."],
	["login_fail_max", "Security", 5.0, 1.0, 100.0, "tries", "head", "Wrong passwords allowed per window before login is blocked."],
	["login_fail_window_min", "Security", 5.0, 1.0, 1440.0, "min", "head", "Window for counting wrong passwords."],
];
const tuneDb = new Map<string, number>();
const TUNE_ENV = new Set(['ring_max']); // pretend CK_TUNE_RING_MAX is set: shown read-only
const tuneVal = (k: string) => tuneDb.get(k) ?? KNOBS.find((x) => x[0] === k)![2];
const tunables = () => KNOBS.map(([key, group, def, min, max, unit, scope, help]) => ({ key, group, value: tuneVal(key), default: def, min, max, unit, scope, help, source: TUNE_ENV.has(key) ? 'env' : tuneDb.has(key) ? 'db' : 'default' }));
/** Head choice (GET /ha/candidates): speed, memory, stability and freedom, weighted by head_w_*; best first. */
function candidates() {
	const w = { speed: tuneVal('head_w_speed'), memory: tuneVal('head_w_memory'), stability: tuneVal('head_w_stability'), free: tuneVal('head_w_free') };
	const sum = w.speed + w.memory + w.stability + w.free || 1;
	const lostLast = (id: string) => events.filter((e) => e.kind === 'node.lost' && e.detail.includes(nodes.get(id)!.host)).length;
	const standbyIds = new Set(ha.standbys.map((s) => s.node_id));
	return [...nodes.values()].map((n, i) => {
		const inv = (n.inventory ?? {}) as any;
		const parts = { speed: Math.min(1, (n.calib_mhs ?? 0) / 70), memory: Math.min(1, (inv.ram_bytes / 2 ** 30) / 32 * 0.6 + (inv.cores_logical / 16) * 0.4), stability: Math.max(0, 1 - lostLast(n.node_id) * 0.2 - (i % 7 === 0 ? 0.25 : 0)), free: n.presence === 'busy' ? 0 : 1 };
		const score = Math.round(100 * (parts.speed * w.speed + parts.memory * w.memory + parts.stability * w.stability + parts.free * w.free) / sum * 10) / 10;
		const why = n.node_id === ha.head || standbyIds.has(n.node_id) ? '' : i >= 28 ? 'virtual PC' : !n.online ? (n.power === 'asleep' ? 'asleep' : 'not connected') : n.state !== 'active' ? `not active (${n.state})` : !matchIds(haCfg.selector).includes(n.node_id) ? 'does not match the standby selector' : '';
		const role = n.node_id === ha.head ? 'head' : standbyIds.has(n.node_id) ? 'standby' : 'candidate';
		const lab = labs.find((l) => l.id === n.lab_id)?.name ?? null;
		return { node_id: n.node_id, host: n.host, lab, score, parts, role, eligible: why === '', why };
	}).sort((a, b) => b.score - a.score);
}

// watts per lab from the same model sim.rs uses: idle 25 W, busy 65 W, sleep 2 W, boot 65 W
const labWatts = () => labs.map((l) => {
	const ns = [...nodes.values()].filter((n) => n.lab_id === l.id);
	const c = { awake: 0, asleep: 0, waking: 0 };
	let watts = 0;
	for (const n of ns) { c[n.power]++; watts += n.power === 'asleep' ? 2 : n.power === 'waking' ? 65 : n.presence === 'busy' ? 65 : 25; }
	return { lab: l.name, watts, ...c };
});
// ---- POST /api/v1/sim: a plausible stand-in for rust/src/sim.rs (same JSON as `ck sim` writes: summary + timeline)
let simBusy = false;
function rng(seed: number) { let s = seed >>> 0 || 1; return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32); }
function simJob(b: any, pcs: number) {
	const r = rng(b.seed ?? 1);
	const rate = b.nodes.reduce((a: number, g: any) => a + (g.count ?? 1) * (g.rate ?? 1e6) * (1 - (g.jitter ?? 0) / 4), 0);
	const n = b.job.n, ideal = n / rate;
	const fails = b.nodes.reduce((a: number, g: any) => a + (g.mtbf_s > 0 ? Math.round(((g.count ?? 1) * ideal) / g.mtbf_s) : 0), 0);
	const headFail = b.head_failure?.at_s != null && b.head_failure.at_s < ideal ? (b.architecture === 'single' ? (b.head_failure.manual_restart_s ?? 100) : (b.head_failure.recovery_s ?? 6)) : 0;
	const slow = 1 + 0.015 + (b.architecture === 'single' ? 0.02 : 0) + Math.min(0.08, pcs / 150_000) + fails * 2e-5;
	const end = ideal * slow + headFail, sample = b.sample_s ?? Math.max(1, end / 200);
	const tl: number[][] = [];
	for (let t = 0, done = 0; t <= end + sample; t += sample) {
		const ramp = Math.min(1, t / Math.max(sample * 2, 1)), stall = headFail && t > b.head_failure.at_s && t < b.head_failure.at_s + headFail ? 0 : 1;
		const up = Math.round(pcs * (1 - 0.01 * r()) * (stall ? 1 : 0.97));
		const rt = Math.min(rate * ramp * stall * (0.97 + 0.03 * r()), (n - done) / sample);
		done += rt * sample;
		tl.push([+t.toFixed(3), Math.round(done), Math.round(rt), Math.round(up * (done >= n ? 0 : 0.96 + 0.04 * r())), up, Math.min(1, 0.05 + pcs / 60_000 + 0.02 * r())]);
		if (done >= n) break;
	}
	return {
		summary: { finished: true, makespan_s: end, end_s: end, ideal_s: ideal, efficiency_pct: 100 * ideal / end, n, done: n, tasks: Math.round(n / 5e5), head_utilisation_pct: Math.min(100, 5 + pcs / 600), node_failures: fails, recovered_work: fails * 4_000_000, duplicate_work: 0, throughput_per_s: n / end, tasks_per_s: Math.round(n / 5e5) / end },
		series: tl
	};
}
function simDay(b: any, pcs: number) {
	const hours = b.day.hours ?? 24, step = 300, eco = !!b.eco?.enabled, pw = { idle: 25, busy: 65, sleep: 2, ...b.power };
	const lines = ['t_s,asleep,booting,awake_idle,busy,class,power_w'];
	let wh = 0, wakes = 0, prev = 0;
	for (let t = 0; t <= hours * 3600; t += step) {
		const h = (t / 3600) % 24, work = h >= 1 && h < 9 ? 0.85 : 0.1, cls = h >= 9 && h < 17 ? Math.min(pcs, 60) : 0;
		const busy = Math.round(pcs * work), idle0 = pcs - busy - cls;
		const asleep = eco ? Math.round(idle0 * (h >= 17 || h < 8 ? 0.9 : 0.5)) : 0, booting = eco && Math.abs(asleep - prev) > pcs * 0.2 ? Math.round(pcs * 0.05) : 0;
		if (asleep < prev) wakes += prev - asleep;
		prev = asleep;
		const idle = Math.max(0, idle0 - asleep - booting), w = busy * pw.busy + idle * pw.idle + asleep * pw.sleep + booting * 65 + cls * 45;
		wh += (w * step) / 3600;
		lines.push([t, asleep, booting, idle, busy, cls, Math.round(w)].join(','));
	}
	const ch = (x: number) => +(wh * x / 1000).toFixed(2);
	return {
		kwh: ch(1), kwh_busy: ch(0.55), kwh_idle: ch(eco ? 0.3 : 0.45), kwh_sleep: ch(eco ? 0.05 : 0), kwh_boot: ch(eco ? 0.04 : 0), kwh_class: ch(0.06), node_failures: 0, head_failures: 0, lost_work: 0, wakes, class_delay_pc_s: 0, class_delay_max_s: eco ? 45 : 0, hours,
		jobs: (b.jobs ?? []).map((j: any) => ({ name: j.name, arrive_s: (j.arrive_h ?? 0) * 3600, started_s: (j.arrive_h ?? 0) * 3600 + 30, finished_s: (j.arrive_h ?? 0) * 3600 + 5400, wait_s: 30, makespan_s: 5430, run_s: 5400, pc_seconds: pcs * 5400, peak_pcs: pcs, done: j.work, left: 0, marginal_busy_kwh: 3.2 })),
		_timeline: lines.join('\n') + '\n'
	};
}
const json = (body: unknown, status = 200, headers: Record<string, string> = {}) =>
	new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', ...headers } });
const err = (status: number, code: string, message: string) => json({ error: { code, message } }, status);

function who(req: Request): { name: string; role: Role } | null {
	if (MODE === 'open') return { name: 'anonymous', role: 'admin' };
	const c = /ck_session=([^;]+)/.exec(req.headers.get('cookie') ?? '')?.[1];
	const n = c ? sessions.get(c) : undefined;
	const u = n ? users.get(n) : undefined;
	return n && u ? { name: n, role: u.role } : null;
}
const cookieFor = (name: string) => {
	const t = crypto.randomUUID();
	sessions.set(t, name);
	return { 'set-cookie': `ck_session=${t}; HttpOnly; SameSite=Strict; Path=/` };
};

function page<T extends { id: number }>(all: T[], url: URL) {
	const limit = Math.min(500, Number(url.searchParams.get('limit') ?? 50));
	const before = url.searchParams.get('before');
	const sorted = [...all].sort((a, b) => b.id - a.id).filter((x) => before == null || x.id < Number(before));
	const rows = sorted.slice(0, limit);
	return { rows, next: sorted.length > limit ? rows[rows.length - 1].id : null };
}

function resultText(j: Job, red?: string) {
	if (j.kind === 'list') {
		const its = j.items ?? [];
		const r = red ?? j.reducer;
		if (r === 'sort') return [...its].sort().join('\n');
		if (r === 'count') return String(its.length);
		if (r.startsWith('top:')) return [...its].sort().reverse().slice(0, Number(r.slice(4))).join('\n');
		if (r === 'sum') return String(its.reduce((a, x) => a + x.length, 0));
		return its.map((x) => `${x} ${Bun.SHA256.hash(x, 'hex').slice(0, 16)}`).join('\n');
	}
	if (j.kind === 'synth') return String(j.total);
	if (j.kind === 'sha') return Array.from({ length: 6 }, (_, i) => `${j.from + i * 7_919_113} ${Bun.SHA256.hash(String(i), 'hex')}`).join('\n');
	return 'part 1: ok\npart 2: ok\n';
}

async function handle(req: Request): Promise<Response> {
	const url = new URL(req.url);
	const p = url.pathname;
	const m = req.method;
	if (url.searchParams.has('pcs')) setFleet(Number(url.searchParams.get('pcs')));
	const body = async () => (await req.json().catch(() => ({}))) as Record<string, any>;

	if (p === '/stats') return err(404, 'not_found', 'stats is not mocked');
	if (p === '/api/v1/me') {
		const u = who(req);
		return json({ user: MODE === 'open' ? null : u, mode: MODE === 'open' ? 'open' : 'required', needs_bootstrap: MODE === 'bootstrap' && users.size === 0 });
	}
	if (p === '/api/v1/bootstrap' && m === 'POST') {
		const b = await body();
		if (users.size > 0) return err(409, 'conflict', 'A user already exists.');
		if (String(b.password ?? '').length < 12) return err(400, 'invalid_request', 'Password must be at least 12 characters.');
		users.set(b.name, { password: b.password, role: 'admin', created_ms: now() });
		logEvent('auth.bootstrap', b.name, 'first admin created');
		return json({ user: { name: b.name, role: 'admin' } }, 200, cookieFor(b.name));
	}
	if (p === '/api/v1/login' && m === 'POST') {
		const b = await body();
		const u = users.get(b.name);
		if (!u || u.password !== b.password) return err(401, 'unauthorized', 'Wrong name or password.');
		logEvent('auth.login', b.name, 'login');
		return json({ user: { name: b.name, role: u.role } }, 200, cookieFor(b.name));
	}
	if (p === '/api/v1/logout' && m === 'POST') {
		const c = /ck_session=([^;]+)/.exec(req.headers.get('cookie') ?? '')?.[1];
		if (c) sessions.delete(c);
		return json({}, 200, { 'set-cookie': 'ck_session=; Max-Age=0; Path=/' });
	}

	const me = who(req);
	if (!me) return err(401, 'unauthorized', 'Sign in first.');
	const need = (r: Role) => (RANK[me.role] >= RANK[r] ? null : err(403, 'forbidden', `This needs the ${r} role.`));
	let d: Response | null;

	if (p === '/api/v1/tokens' && m === 'POST') return json({ token: `ckt_${crypto.randomUUID().replaceAll('-', '')}` });
	if (p === '/api/v1/users') {
		if ((d = need('admin'))) return d;
		if (m === 'GET') return json({ users: [...users].map(([name, u]) => ({ name, role: u.role, created_ms: u.created_ms, quota: u.quota ?? { max_pcs: 0, max_running: 0 } })) });
		if (m === 'POST') {
			const b = await body();
			if (!b.name || String(b.password ?? '').length < 12) return err(400, 'invalid_request', 'Name required and password at least 12 characters.');
			if (users.has(b.name)) return err(409, 'conflict', `User ${b.name} already exists.`);
			users.set(b.name, { password: b.password, role: b.role, created_ms: now() });
			logEvent('user.create', me.name, `created ${b.name} as ${b.role}`);
			return json({ user: { name: b.name, role: b.role } });
		}
	}
	if (p === '/api/v1/cluster') return json(cluster());
	if (p === '/api/v1/ha') return json(haState());
	if (p === '/api/v1/tunables') {
		if (m === 'PUT') {
			if ((d = need('admin'))) return d;
			const b = await body();
			const next = new Map(tuneDb);
			for (const [k, v] of Object.entries(b)) {
				const kn = KNOBS.find((x) => x[0] === k);
				if (!kn) return err(400, 'invalid_request', `Unknown setting ${k}.`);
				if (TUNE_ENV.has(k)) return err(409, 'conflict', `${k} is set by the environment (CK_TUNE_${k.toUpperCase()}) and cannot be changed here.`);
				if (v === null) next.delete(k);
				else if (typeof v !== 'number' || !(v >= kn[3] && v <= kn[4])) return err(400, 'invalid_request', `${k} must be between ${kn[3]} and ${kn[4]} ${kn[5]}.`);
				else next.set(k, v);
			}
			const before = tunables();
			tuneDb.clear();
			next.forEach((v, k) => tuneDb.set(k, v));
			const ch = tunables().flatMap((t, i) => (t.value !== before[i].value ? [`${t.key} ${before[i].value} -> ${t.value}`] : []));
			logEvent('tune.changed', me.name, ch.join(', ') || 'no change');
		}
		return json({ tunables: tunables() });
	}
	if (p === '/api/v1/logs') {
		if ((d = need('operator'))) return d;
		const since = Number(url.searchParams.get('since_ms') ?? 0), kind = url.searchParams.get('kind') ?? '', role = url.searchParams.get('role') ?? '';
		const lv = url.searchParams.get('level');
		if (lv && !(lv in LV)) return err(400, 'invalid_request', 'level is error|warn|info|debug');
		const max = lv ? LV[lv as keyof typeof LV] : 3, limit = Math.max(1, Math.min(20_000, Number(url.searchParams.get('limit') ?? 200)));
		const out: LogLine[] = [];
		for (let i = logLines.length - 1; i >= 0 && out.length < limit; i--) {
			const l = logLines[i];
			if (l.ts_ms >= since && LV[l.level] <= max && (!kind || l.kind.startsWith(kind)) && (!role || l.role === role)) out.push(l);
		}
		return json({ logs: out });
	}
	if (p === '/api/v1/ha/candidates') return json({ candidates: candidates() });
	if (p === '/api/v1/ha/switchover' && m === 'POST') {
			if ((d = need('admin'))) return d;
			const b = await body();
			if (!ha.standbys.some((s) => s.node_id === b.to) || ha.role !== 'primary') return err(409, 'conflict', 'That PC is not a connected standby.');
			logEvent('ha.switchover', me.name, `handing over to ${hostOf(b.to)}`);
			failover(0, b.to);
			return json({ to: nodes.get(b.to)?.addr ?? b.to }, 202);
		}
	if (p === '/api/v1/templates') {
		if (m === 'GET') return json({ templates });
		if (m === 'POST') {
			if ((d = need('researcher'))) return d;
			const b = await body();
			if (!/^[a-z0-9][a-z0-9_-]{0,39}$/i.test(String(b.name ?? '')) || !b.spec || typeof b.spec !== 'object') return err(400, 'invalid_request', 'Give a name (letters, digits, - and _) and a spec.');
			const at = templates.findIndex((t) => t.name === b.name);
			if (at >= 0 && templates[at].owner !== me.name && me.role !== 'admin') return err(403, 'forbidden', 'Only the owner or an admin can replace that template.');
			const row = { name: b.name, spec: b.spec, note: String(b.note ?? ''), owner: at >= 0 ? templates[at].owner : me.name, created_ms: now() };
			if (at >= 0) templates[at] = row; else templates.push(row);
			return json({ template: row });
		}
	}
	const tm = /^\/api\/v1\/templates\/([^/]+)$/.exec(p);
	if (tm && m === 'DELETE') {
		if ((d = need('researcher'))) return d;
		const at = templates.findIndex((t) => t.name === decodeURIComponent(tm[1]));
		if (at < 0) return err(404, 'not_found', 'No such template.');
		if (templates[at].owner !== me.name && me.role !== 'admin') return err(403, 'forbidden', 'Only the owner or an admin can delete that template.');
		templates.splice(at, 1);
		return json({});
	}
	if (p === '/api/v1/nodes/identify' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const ids: string[] = Array.isArray(b.nodes) ? b.nodes : [];
		if (!ids.length || ids.some((i) => !nodes.has(i)) || !(b.secs >= 1 && b.secs <= 600)) return err(400, 'invalid_request', 'nodes and secs 1 to 600 are required.');
		for (const i of ids) setNode(i, { identify_until_ms: now() + b.secs * 1000 });
		setTimeout(() => ids.forEach((i) => setNode(i, {})), b.secs * 1000 + 100);
		return json({ nodes: ids.map((i) => wire(nodes.get(i)!)) });
	}
	const qm = /^\/api\/v1\/users\/([^/]+)\/quota$/.exec(p);
	if (qm && m === 'PUT') {
		if ((d = need('admin'))) return d;
		const u = users.get(decodeURIComponent(qm[1]));
		const b = await body();
		if (!u) return err(404, 'not_found', 'No such user.');
		if (!(Number.isInteger(b.max_pcs) && b.max_pcs >= 0) || !(Number.isInteger(b.max_running) && b.max_running >= 0)) return err(400, 'invalid_request', 'max_pcs and max_running are whole numbers, 0 or more.');
		u.quota = { max_pcs: b.max_pcs, max_running: b.max_running };
		logEvent('user.quota', me.name, `${decodeURIComponent(qm[1])}: ${b.max_pcs} PCs, ${b.max_running} running`);
		return json({ user: { name: decodeURIComponent(qm[1]), role: u.role, quota: u.quota } });
	}
	if (/^\/api\/v1\/blobs\/[0-9a-f]{64}$/.test(p) && m === 'GET') return new Response('col_a,col_b\n1,2\n3,4\n', { headers: { 'content-type': 'application/octet-stream' } });
	if (p === '/api/v1/net' && m === 'GET') return json(netJson());
	if (p === '/api/v1/net/probe' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		if (b.burst) setTimeout(() => (uplinkProbed = true), 1500);
		return json({ queued: [...nodes.values()].filter((n) => n.online).length });
	}
	if (p === '/api/v1/nodes' && m === 'GET') return json({ nodes: [...nodes.values()].map(wire) });
	const nm = /^\/api\/v1\/nodes\/([^/]+)(\/place)?$/.exec(p);
	if (nm && m === 'GET' && !nm[2]) {
		const n = nodes.get(nm[1]);
		return n ? json({ node: wire(n) }) : err(404, 'not_found', 'No such PC.');
	}
	if (p === '/api/v1/nodes/power' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const ids: string[] = Array.isArray(b.node_ids) ? b.node_ids : [];
		if (!ids.length || !['sleep', 'wake'].includes(b.action) || ids.some((i) => !nodes.has(i))) return err(400, 'invalid_request', 'node_ids and action sleep or wake are required.');
		if (b.action === 'sleep' && ids.some((i) => !nodes.get(i)!.online)) return err(409, 'conflict', 'A selected PC is not connected.');
		for (const i of ids) {
			if (b.action === 'sleep') setNode(i, { online: false, power: 'asleep' });
			else {
				setNode(i, { power: 'waking' });
				setTimeout(() => setNode(i, { online: true, power: 'awake' }), 4_000);
			}
			logEvent(b.action === 'sleep' ? 'power.sleep' : 'power.wake', me.name, `${nodes.get(i)!.host} ${b.action === 'sleep' ? 'went to sleep' : 'is waking'}`);
		}
		return json({ nodes: ids.map((i) => nodes.get(i)) });
	}
	if (p === '/api/v1/power') {
		if (m === 'PUT') {
			if ((d = need('operator'))) return d;
			const b = await body();
			if (!['off', 'eco'].includes(b.mode) || !(b.idle_min >= 1 && b.idle_min <= 1440) || !(b.keep_awake >= 0 && b.keep_awake <= 10000)) return err(400, 'invalid_request', 'mode off or eco, idle_min 1-1440, keep_awake 0-10000.');
			powerCfg = { mode: b.mode, idle_min: b.idle_min, keep_awake: b.keep_awake, wake_on_demand: !!b.wake_on_demand };
			logEvent('power.settings', me.name, `${powerCfg.mode}, idle ${powerCfg.idle_min} min`);
		}
		return json({ power: powerCfg, asleep: [...nodes.values()].filter((n) => n.power === 'asleep').map((n) => n.node_id), labs: labWatts() });
	}
	if (p === '/api/v1/ha/settings') {
		if (m === 'PUT') {
			if ((d = need('admin'))) return d;
			const b = await body();
			if (!(Number.isInteger(b.target_standbys) && b.target_standbys >= 0 && b.target_standbys <= 5)) return err(400, 'invalid_request', 'target_standbys is 0-5.');
			const f = parseSel(String(b.selector ?? ''));
			if (typeof f === 'string') return err(400, 'invalid_request', f);
			haCfg = { target_standbys: b.target_standbys, selector: String(b.selector ?? '') };
		}
		return json({ settings: haCfg });
	}
	if (p === '/api/v1/nodes/state' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const ids: string[] = Array.isArray(b.node_ids) ? b.node_ids : [];
		if (!ids.length || !['active', 'cordoned', 'draining', 'maintenance', 'retired'].includes(b.state) || ids.some((i) => !nodes.has(i)))
			return err(400, 'invalid_request', 'node_ids and a valid state are required.');
		for (const i of ids) {
			setNode(i, { state: b.state, state_reason: String(b.reason ?? ''), state_by: me.name });
			logEvent(nodes.get(i)!.state === 'active' ? 'node.approve' : 'node.state', me.name, `${nodes.get(i)!.host} -> ${b.state}${b.reason ? ` (${b.reason})` : ''}`);
		}
		return json({ nodes: ids.map((i) => nodes.get(i)) });
	}
	if (nm && nm[2] && m === 'PUT') {
		if ((d = need('operator'))) return d;
		const n = nodes.get(nm[1]);
		if (!n) return err(404, 'not_found', 'No such PC.');
		const b = await body();
		if (b.lab_id == null) setNode(n.node_id, { lab_id: null, x: null, y: null });
		else {
			const lab = labs.find((l) => l.id === b.lab_id);
			if (!lab || !(b.x >= 0 && b.x < lab.w && b.y >= 0 && b.y < lab.h)) return err(400, 'invalid_request', 'That seat is outside the lab.');
			if ([...nodes.values()].some((o) => o !== n && o.lab_id === lab.id && o.x === b.x && o.y === b.y)) return err(409, 'conflict', 'seat taken');
			setNode(n.node_id, { lab_id: lab.id, x: b.x, y: b.y });
		}
		return json({ node: n });
	}
	if (p === '/api/v1/select' && m === 'GET') {
		const q = url.searchParams.get('q') ?? '';
		const f = parseSel(q);
		if (typeof f === 'string') return json({ count: 0, node_ids: [], error: f });
		const ids = matchIds(q);
		return json({ count: ids.length, node_ids: ids, error: null });
	}
	const lbm = /^\/api\/v1\/nodes\/([^/]+)\/labels$/.exec(p);
	if (lbm && m === 'PUT') {
		if ((d = need('operator'))) return d;
		const n = nodes.get(lbm[1]);
		if (!n) return err(404, 'not_found', 'No such PC.');
		const b = await body();
		const labels = { ...n.labels };
		for (const [k, v] of Object.entries(b)) { if (v === null) delete labels[k]; else labels[k] = String(v); }
		setNode(n.node_id, { labels });
		logEvent('node.labels', me.name, `${n.host} labels ${JSON.stringify(labels)}`);
		return json({ node: n });
	}
	if (p === '/api/v1/pools' && m === 'GET') return json({ pools: [...pools.values()] });
	const plm = /^\/api\/v1\/pools\/([^/]+)$/.exec(p);
	if (plm && (m === 'PUT' || m === 'DELETE')) {
		if ((d = need('operator'))) return d;
		const name = decodeURIComponent(plm[1]);
		if (m === 'DELETE') { if (!pools.delete(name)) return err(404, 'not_found', 'No such pool.'); }
		else {
			const b = await body();
			if (!/^[a-z0-9][a-z0-9_-]{0,39}$/i.test(name)) return err(400, 'invalid_request', 'Pool names use letters, digits, - and _.');
			if (!Array.isArray(b.members) || b.members.some((i: string) => !nodes.has(i))) return err(400, 'invalid_request', 'members must be known node ids.');
			pools.set(name, { name, members: b.members, note: String(b.note ?? '') });
		}
		syncPools();
		for (const n of nodes.values()) sendAll('node', n);
		logEvent('pool.change', me.name, `${m} ${name}`);
		return json(m === 'DELETE' ? {} : { pool: pools.get(name) });
	}
	if (p === '/api/v1/caps' && m === 'GET') return json({ caps });
	if (p === '/api/v1/caps' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const f = parseSel(String(b.selector ?? ''));
		if (typeof f === 'string') return err(400, 'invalid_request', f);
		if (!(b.max_threads >= 1)) return err(400, 'invalid_request', 'max_threads must be 1 or more.');
		const cap = { id: ++capSeq, selector: String(b.selector ?? ''), max_threads: b.max_threads, note: String(b.note ?? '') };
		caps.push(cap);
		return json({ cap });
	}
	const cdm = /^\/api\/v1\/(caps|reserves)\/(\d+)$/.exec(p);
	if (cdm && m === 'DELETE') {
		if ((d = need('operator'))) return d;
		const list = (cdm[1] === 'caps' ? caps : reserves) as { id: number }[];
		const i = list.findIndex((x) => x.id === Number(cdm[2]));
		if (i < 0) return err(404, 'not_found', 'No such entry.');
		list.splice(i, 1);
		return json({});
	}
	if (p === '/api/v1/reserves' && m === 'GET') return json({ reserves: reserves.map((r) => ({ ...r, held: heldFor(r) })) });
	if (p === '/api/v1/reserves' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const f = parseSel(String(b.selector ?? ''));
		if (typeof f === 'string') return err(400, 'invalid_request', f);
		const r = { id: ++resSeq, selector: String(b.selector ?? ''), keep_n: b.keep_n ?? 0, keep_pct: b.keep_pct ?? 0, note: String(b.note ?? '') };
		if (!(r.keep_n > 0 || r.keep_pct > 0)) return err(400, 'invalid_request', 'keep_n or keep_pct is required.');
		reserves.push(r);
		return json({ reserve: { ...r, held: heldFor(r) } });
	}
	if (p === '/api/v1/windows' && m === 'GET') return json({ windows: wins.map(winRow) });
	if (p === '/api/v1/windows' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const f = parseSel(String(b.selector ?? ''));
		if (typeof f === 'string') return err(400, 'invalid_request', f);
		if (!/^\d{2}:\d{2}$/.test(b.start) || !/^\d{2}:\d{2}$/.test(b.end) || b.start === b.end) return err(400, 'invalid_request', 'start and end must be different HH:MM times.');
		if (!String(b.days ?? '').split(',').every((t: string) => t.split('-').every((x) => DAYN.includes(x)))) return err(400, 'invalid_request', 'days like mon-fri or sat,sun.');
		if (b.action !== 'block' && !(b.cap_threads >= 1)) return err(400, 'invalid_request', 'A cap window needs cap_threads of 1 or more.');
		const w: Win = { id: ++winSeq, selector: String(b.selector ?? ''), days: b.days, start: b.start, end: b.end, action: b.action === 'cap' ? 'cap' : 'block', cap_threads: b.action === 'cap' ? b.cap_threads : null, note: String(b.note ?? '') };
		wins.push(w);
		logEvent('window.add', me.name, `window ${w.id}`);
		return json({ window: winRow(w) });
	}
	const wdm = /^\/api\/v1\/(windows|reservations)\/(\d+)$/.exec(p);
	if (wdm && m === 'DELETE') {
		if ((d = need('operator'))) return d;
		const list = (wdm[1] === 'windows' ? wins : resvs) as { id: number }[];
		const i = list.findIndex((x) => x.id === Number(wdm[2]));
		if (i < 0) return err(404, 'not_found', 'No such entry.');
		list.splice(i, 1);
		return json({});
	}
	if (p === '/api/v1/reservations' && m === 'GET') return json({ reservations: resvs.filter((r) => r.end_ms > now()).map(resvRow) });
	if (p === '/api/v1/reservations' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const f = parseSel(String(b.selector ?? ''));
		if (typeof f === 'string') return err(400, 'invalid_request', f);
		if (!(b.end_ms > b.start_ms)) return err(400, 'invalid_request', 'end_ms must be after start_ms.');
		const r: Resv = { id: ++rsvSeq, selector: String(b.selector ?? ''), start_ms: b.start_ms, end_ms: b.end_ms, note: String(b.note ?? '') };
		resvs.push(r);
		return json({ reservation: resvRow(r) });
	}
	if (p === '/api/v1/estimate' && m === 'GET') {
		const n = Number(url.searchParams.get('n'));
		const sel = url.searchParams.get('selector') ?? '';
		const f = parseSel(sel);
		if (typeof f === 'string') return err(400, 'invalid_request', f);
		if (!(n > 0)) return err(400, 'invalid_request', 'n must be positive.');
		const ids = matchIds(sel).filter((i) => takes(i));
		const fleet_rate = ids.length * 8 * 5e6;
		return json({ matching_pcs: ids.length, fleet_rate, eta_secs: ids.length ? (url.searchParams.get('kind') === 'list' ? n * 2 : n / fleet_rate) + 12 : null });
	}
	if (p === '/api/v1/config') {
		if ((d = need('admin'))) return d;
		if (m === 'GET') return json({ version: 1, labs, placements: [...nodes.values()].filter((n) => n.lab_id).map((n) => ({ node_id: n.node_id, lab_id: n.lab_id, x: n.x, y: n.y })), labels: [...nodes.values()].filter((n) => Object.keys(n.labels).length).map((n) => ({ node_id: n.node_id, labels: n.labels })), pools: [...pools.values()], caps, reserves });
		if (m === 'POST') {
			const b = await body();
			if (b.version !== 1) return err(400, 'invalid_request', 'Unsupported config version.');
			const changes: string[] = [];
			for (const np of (b.pools ?? []) as { name: string; members: string[] }[]) {
				const old = pools.get(np.name);
				if (!old) changes.push(`+ pool ${np.name} (${np.members.length} PCs)`);
				else if (old.members.join() !== np.members.join()) changes.push(`~ pool ${np.name}: ${old.members.length} -> ${np.members.length} PCs`);
			}
			for (const op of pools.keys()) if (!(b.pools ?? []).some((x: { name: string }) => x.name === op)) changes.push(`- pool ${op}`);
			for (const c of (b.caps ?? []) as { selector: string; max_threads: number }[]) if (!caps.some((x) => x.selector === c.selector)) changes.push(`+ cap ${c.selector || 'all'} max ${c.max_threads} threads`);
			if (url.searchParams.get('dry_run')) return json({ changes });
			logEvent('config.import', me.name, `${changes.length} changes`);
			return json({ changes });
		}
	}
	if (p === '/api/v1/labs' && m === 'GET') return json({ labs });
	if (p === '/api/v1/labs' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const ok = (v: unknown) => Number.isInteger(v) && (v as number) >= 1 && (v as number) <= 64;
		if (!String(b.name ?? '').trim() || !ok(b.w) || !ok(b.h)) return err(400, 'invalid_request', 'A name and a width and height of 1 to 64 are required.');
		const lab = { id: ++labSeq, name: String(b.name).trim(), w: b.w, h: b.h, created_ms: now() };
		labs.push(lab);
		logEvent('lab.create', me.name, lab.name);
		return json({ lab });
	}
	const lm = /^\/api\/v1\/labs\/(\d+)$/.exec(p);
	if (lm && (m === 'PATCH' || m === 'DELETE')) {
		if ((d = need('operator'))) return d;
		const lab = labs.find((l) => l.id === Number(lm[1]));
		if (!lab) return err(404, 'not_found', 'No such lab.');
		if (m === 'DELETE') {
			labs.splice(labs.indexOf(lab), 1);
			for (const n of nodes.values()) if (n.lab_id === lab.id) setNode(n.node_id, { lab_id: null, x: null, y: null });
			return json({});
		}
		const b = await body();
		const w = b.w ?? lab.w, h = b.h ?? lab.h;
		if (!(w >= 1 && w <= 64 && h >= 1 && h <= 64) || !String(b.name ?? lab.name).trim()) return err(400, 'invalid_request', 'Name required, width and height 1 to 64.');
		Object.assign(lab, { name: String(b.name ?? lab.name).trim(), w, h });
		for (const n of nodes.values()) if (n.lab_id === lab.id && ((n.x ?? 0) >= w || (n.y ?? 0) >= h)) setNode(n.node_id, { lab_id: null, x: null, y: null });
		return json({ lab });
	}
	if (p === '/api/v1/join-codes' && m === 'POST') {
		if ((d = need('operator'))) return d;
		const b = await body();
		const part = () => Array.from({ length: 4 }, () => 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789'[Math.floor(Math.random() * 32)]).join('');
		const uses = b.uses ?? 50, ttl = b.ttl_min ?? 120;
		if (!(uses >= 1 && uses <= 500 && ttl >= 1 && ttl <= 10080)) return err(400, 'invalid_request', 'uses 1 to 500, ttl_min 1 to 10080.');
		logEvent('joincode.create', me.name, `${uses} uses`);
		return json({ code: `${part()}-${part()}-${part()}`, expires_ms: now() + ttl * 60_000, uses, auto_approve: !!b.auto_approve });
	}

	if (p === '/api/v1/jobs' && m === 'GET') {
		const { rows, next } = page(jobs, url);
		return json({ jobs: rows, next });
	}
	if (p === '/api/v1/jobs' && m === 'POST') {
		if ((d = need('researcher'))) return d;
		let b = await body();
		if (b.template) {
			const t = templates.find((x) => x.name === b.template);
			if (!t) return err(404, 'not_found', `No template ${b.template}.`);
			const { template: _t, ...over } = b;
			b = { ...t.spec, ...over };
		}
		if (!['sha', 'cmd', 'synth', 'list'].includes(b.kind) || !(b.n > 0)) return err(400, 'invalid_request', 'kind and a positive n are required.');
		if ((b.kind === 'cmd' || b.kind === 'list') && (d = need('operator'))) return d;
		if (b.kind === 'list' && !(Array.isArray(b.items) && b.items.length)) return err(400, 'invalid_request', 'A list job needs items.');
		if (!b[b.kind === 'synth' ? 'synth_ns' : b.kind === 'list' ? 'cmd' : b.kind]) return err(400, 'invalid_request', `Give the ${b.kind} field.`);
		if (b.selector) { const f = parseSel(String(b.selector)); if (typeof f === 'string') return err(400, 'invalid_request', f); }
		if (b.placement && !['spread', 'pack', 'fastest', 'efficient'].includes(b.placement)) return err(400, 'invalid_request', 'placement must be spread, pack, fastest or efficient.');
		const pr = ['urgent', 'high', 'normal', 'low', 'background'].indexOf(b.priority ?? 'normal');
		if (pr < 0) return err(400, 'invalid_request', 'priority must be urgent, high, normal, low or background.');
		const j = mkJob({ kind: b.kind, total: b.n, from: b.from ?? 1, chunk: b.chunk ?? 0, name: b.name ?? '', submitter: me.name, selector: String(b.selector ?? '').trim(), priority: pr, policy: b.policy ?? 'adaptive', reducer: b.reducer ?? '', items: b.items, max_pcs: b.max_pcs ?? 0, placement: b.placement ?? '', mem_mb: b.mem_mb ?? 0 });
		if (!running()) { j.state = 'running'; j.started_ms = now(); }
		logEvent('job.submit', me.name, `job ${j.id} submitted`);
		for (const s of subs) s('job', j);
		return json({ job: j });
	}
	const jm = /^\/api\/v1\/jobs\/(\d+)(\/result|\/cancel|\/files)?$/.exec(p);
	if (jm) {
		const j = jobs.find((x) => x.id === Number(jm[1]));
		if (!j) return err(404, 'not_found', `Job ${jm[1]} does not exist.`);
		if (!jm[2]) return json({ job: j, parts: Math.floor(j.done / (j.chunk || 1_000_000)) });
		if (jm[2] === '/files') return json({ files: fileList(j) });
		if (jm[2] === '/result') return new Response(resultText(j, url.searchParams.get('reducer') ?? undefined), { headers: { 'content-type': 'text/plain' } });
		if (m === 'POST') {
			if ((d = need('researcher'))) return d;
			if (RANK[me.role] < RANK.operator && j.submitter !== me.name) return err(403, 'forbidden', 'Only an operator can cancel another person\'s job.');
			if (j.state !== 'queued' && j.state !== 'running') return err(409, 'conflict', 'That job already finished.');
			j.state = 'cancelled'; j.finished_ms = now();
			logEvent('job.cancel', me.name, `job ${j.id} cancelled`);
			const q = jobs.find((x) => x.state === 'queued');
			if (!running() && q) { q.state = 'running'; q.started_ms = now(); }
			for (const s of subs) s('job', j);
			return json({ job: j });
		}
	}
	if (p === '/api/v1/events') {
		const { rows, next } = page(events, url);
		return json({ events: rows, next });
	}
	if (p === '/api/v1/sim' && m === 'POST') {
		if ((d = need('researcher'))) return d;
		const b = await body();
		const day = b && typeof b === 'object' && 'day' in b;
		const groups = (day ? b.pcs : b.nodes) ?? [];
		const pcs = groups.reduce((a: number, g: any) => a + (g.count ?? 1), 0);
		if (!Array.isArray(groups) || (!day && !(b.job?.n > 0))) return err(400, 'invalid_request', 'Give nodes and a job size.');
		if (pcs > 20_000) return err(400, 'invalid_request', 'At most 20000 nodes per run.');
		if (day && (b.day.hours ?? 24) > 48) return err(400, 'invalid_request', 'At most 48 simulated hours per run.');
		if (simBusy) return err(409, 'conflict', 'Another simulation is running.');
		simBusy = true;
		await new Promise((r) => setTimeout(r, 400 + Math.min(1500, pcs / 10)));
		simBusy = false;
		return json(day ? simDay(b, pcs) : simJob(b, pcs));
	}
	if (p === '/api/v1/stream') {
		const enc = new TextEncoder();
		let send: (n: string, d: unknown) => void;
		let timers: ReturnType<typeof setInterval>[] = [];
		const stream = new ReadableStream({
			start(c) {
				send = (n, data) => { try { c.enqueue(enc.encode(`event: ${n}\ndata: ${JSON.stringify(data)}\n\n`)); } catch {} };
				subs.add(send);
				send('cluster', cluster());
				send('ha', haState());
				timers = [setInterval(() => send('cluster', cluster()), 1000), setInterval(() => { try { c.enqueue(enc.encode(': ping\n\n')); } catch {} }, 15000)];
			},
			cancel() { subs.delete(send); timers.forEach(clearInterval); }
		});
		return new Response(stream, { headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' } });
	}
	if (p.startsWith('/api/v1/')) {
	const r = await research({ p, m, url, me, RANK, json, err, body, jobs, workers, now, subs });
	if (r) return r;
	}
	return err(404, 'not_found', `No route ${m} ${p}.`);
}

// SSE `changed` after each successful write of a list (contract)
const WRITES: [RegExp, string[]][] = [[/\/reserves/, ['reserves', 'candidates']], [/\/caps/, ['caps']], [/\/windows/, ['windows', 'timetable']], [/\/reservations/, ['bookings']], [/\/pools/, ['pools']], [/\/templates/, ['templates']], [/\/labels$/, ['labels']], [/\/ha\/settings/, ['candidates']]];
async function serve(req: Request) {
	const res = await handle(req);
	if (req.method !== 'GET' && res.status < 300) for (const [re, whats] of WRITES) if (re.test(new URL(req.url).pathname)) whats.forEach(changed);
	return res;
}
Bun.serve({ port: PORT, hostname: '127.0.0.1', idleTimeout: 0, fetch: serve });
console.log(`mock ck head on http://127.0.0.1:${PORT} (auth: ${MODE})`);
