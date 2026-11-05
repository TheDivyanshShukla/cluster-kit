// Mock of the ck head HTTP API (docs/API.md) for developing and screenshotting the console.
//   MOCK_AUTH=open (default) | required | bootstrap     bun run scripts/mock-api.ts
// Then: CK_DASH=7942 bun run dev -- --port 5174. Logins in "required" mode: admin / adminadminadmin.
// Fake data only; it serves nothing but /api/v1/* and /stats.
type Role = 'viewer' | 'researcher' | 'operator' | 'admin';
const RANK: Record<Role, number> = { viewer: 0, researcher: 1, operator: 2, admin: 3 };
const PORT = Number(process.env.MOCK_PORT ?? 7942);
const MODE = process.env.MOCK_AUTH ?? 'open';

const users = new Map<string, { password: string; role: Role; created_ms: number }>();
if (MODE === 'required') {
	users.set('admin', { password: 'adminadminadmin', role: 'admin', created_ms: Date.now() - 864e5 });
	users.set('vic', { password: 'viewerpass1234', role: 'viewer', created_ms: Date.now() - 3e8 });
	users.set('ana', { password: 'researcherpass1', role: 'researcher', created_ms: Date.now() - 4e8 });
}
const sessions = new Map<string, string>(); // cookie -> user name

type Job = {
	id: number; name: string; kind: 'sha' | 'cmd' | 'synth' | 'list'; from: number; end: number; chunk: number; total: number; done: number;
	state: 'queued' | 'running' | 'done' | 'cancelled' | 'failed'; submitter: string;
	created_ms: number; started_ms: number | null; finished_ms: number | null; error: string | null;
	selector: string; priority: number; policy: string; reducer: string; eta_ms: number | null; items?: string[];
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
const ago = (ms: number) => now() - ms;
mkJob({ name: 'SHA sweep 0-50M', kind: 'sha', total: 50_000_000, done: 50_000_000, state: 'done', created_ms: ago(9e6), started_ms: ago(9e6), finished_ms: ago(8.9e6) });
mkJob({ name: 'Flaky command', kind: 'cmd', total: 8_000_000, done: 3_000_000, state: 'failed', error: 'worker 10.0.0.7 exited with status 2', submitter: 'ana', created_ms: ago(7e6), started_ms: ago(7e6), finished_ms: ago(6.9e6) });
mkJob({ name: 'Old synth', kind: 'synth', total: 1_000_000_000, done: 400_000_000, state: 'cancelled', submitter: 'ana', created_ms: ago(5e6), started_ms: ago(5e6), finished_ms: ago(4.9e6) });
mkJob({ name: 'Synth 500G numbers', kind: 'synth', total: 500_000_000_000, done: 50_000_000_000, state: 'running', created_ms: ago(40_000), started_ms: ago(40_000), selector: 'lab="Lab A"', priority: 2 });
mkJob({ name: 'Exam grading (urgent)', kind: 'sha', total: 80_000_000_000, done: 8_000_000_000, state: 'running', created_ms: ago(30_000), started_ms: ago(30_000), selector: 'pool=night', priority: 0, submitter: 'ana' });
mkJob({ name: 'Hash the word list', kind: 'list', total: 6, done: 6, state: 'done', reducer: 'sort', policy: 'guided', submitter: 'ana', created_ms: ago(3e6), started_ms: ago(3e6), finished_ms: ago(2.9e6), items: ['pear', 'apple', 'fig', 'plum', 'kiwi', 'lime'] });
mkJob({ name: 'Nightly SHA batch', kind: 'sha', total: 200_000_000, submitter: 'ana' });

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
type Node = { node_id: string; host: string; addr: string; inventory: unknown; first_seen_ms: number; last_seen_ms: number; state: NState; state_reason: string; state_by: string; lab_id: number | null; x: number | null; y: number | null; online: boolean; calib_mhs: number | null; presence: 'busy' | 'idle'; labels: Record<string, string>; pools: string[]; class: string; power: 'awake' | 'asleep' | 'waking' };
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
const setNode = (id: string, patch: Partial<Node>) => { const n = nodes.get(id)!; Object.assign(n, patch); sendAll('node', n); };
const takes = (id: string) => { const n = nodes.get(id)!; return n.online && (n.state === 'active' || n.state === 'draining'); };
// states change over time: pc-05 drains then cordons, pc-30 (unplaced) drops off, pc-12 comes back from maintenance
setTimeout(() => setNode('n005', { state: 'draining', state_reason: 'disk check', state_by: 'admin' }), 12_000);
setTimeout(() => setNode('n005', { state: 'cordoned' }), 28_000);
setTimeout(() => { setNode('n030', { online: false }); logEvent('node.lost', '', 'pc-30 lost'); }, 20_000);
Object.assign(nodes.get('n004')!, { online: false, power: 'asleep' }); // an asleep PC from the start
Object.assign(nodes.get('n012')!, { state: 'maintenance', state_reason: 'RAM swap', state_by: 'admin' });

type Ev = { id: number; ts_ms: number; kind: string; actor: string; detail: string };
const events: Ev[] = [];
let evSeq = 0;
const subs = new Set<(name: string, data: unknown) => void>();
const logEvent = (kind: string, actor: string, detail: string, ts = now()) => {
	const e = { id: ++evSeq, ts_ms: ts, kind, actor, detail };
	events.push(e);
	for (const s of subs) s('event', e);
};
for (let i = 0; i < 70; i++) {
	const k = ['node.join', 'job.submit', 'auth.login', 'job.done', 'node.lost'][i % 5];
	logEvent(k, k.startsWith('node') ? '' : i % 2 ? 'ana' : 'admin', `${k} ${['pc-03 joined', 'job 4 submitted', 'login from 127.0.0.1', 'job 1 finished', 'pc-12 lost'][i % 5]}`, ago((70 - i) * 120_000));
}

const runningAll = () => jobs.filter((j) => j.state === 'running');
const running = () => runningAll()[0] ?? null;
function refreshRules() {
	for (const w of workers) {
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
	for (const o of others) { o.done = Math.min(o.total, o.done + 20_000_000 * 0.5 * 30); for (const s of subs) s('job', o); }
	let busy = 0;
	for (const w of workers) {
		w.busy = !!j && takes(w.node_id!) && w.id % 7 !== 0 && !w.blocked && w.presence !== 'busy';
		if (w.busy) busy++;
		w.rate = w.busy ? w.threads * (4.5e6 + Math.random() * 1.5e6) : 0;
		w.cpu = w.busy ? 90 + Math.random() * 9 : 2 + Math.random() * 5;
		w.mem_used = w.mem_total * (0.3 + Math.random() * 0.1);
		if (w.busy) { w.numbers += w.rate * 0.5; w.chunks += Math.random() < 0.1 ? 1 : 0; w.secs += 0.5; }
	}
	if (j) {
		j.done = Math.min(j.total, Math.floor(j.done + workers.reduce((a, w) => a + w.rate, 0) * 0.5));
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
const ha = { role: 'primary' as 'primary' | 'standby' | 'candidate', epoch: 3, standbys: [{ node_id: 'n018', lag: 0 }, { node_id: 'n022', lag: 0 }] };
const hostOf = (id: string) => nodes.get(id)?.host ?? id;
const pushHa = () => sendAll('ha', haState());
const haState = () => {
	const connected = workers.filter((w) => nodes.get(w.node_id!)!.online).length;
	const wob = (id: string) => (id === 'n022' && Math.floor(now() / 1000) % 5 === 0 ? 2 : 0); // a little lag now and then
	const sb = ha.role === 'candidate' ? [] : ha.standbys.map((s) => ({ node_id: s.node_id, lag: Math.max(s.lag, wob(s.node_id)) }));
	return {
		role: ha.role, epoch: ha.epoch, last_seq: haSeq, durable_seq: haSeq - Math.max(0, ...sb.map((s) => s.lag)),
		standbys: sb.map((s) => ({ ...s, acked_seq: haSeq - s.lag })), membership: nodes.size, connected, dispatching: ha.role === 'primary'
	};
};
function recruit() {
	const taken = new Set(ha.standbys.map((s) => s.node_id));
	const id = matchIds(haCfg.selector).find((i) => nodes.get(i)!.online && nodes.get(i)!.power === 'awake' && !taken.has(i));
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
			ha.role = 'primary';
			ha.epoch++;
			logEvent('ha.promote', '', `${head ? hostOf(head.node_id) : 'a standby'} is the head, term ${ha.epoch}`);
			pushHa();
			setTimeout(recruit, 6_000);
		}, 8_000);
	}, after);
}
failover(30_000);
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
		if (m === 'GET') return json({ users: [...users].map(([name, u]) => ({ name, role: u.role, created_ms: u.created_ms })) });
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
	if (p === '/api/v1/ha/switchover' && m === 'POST') {
			if ((d = need('admin'))) return d;
			const b = await body();
			if (!ha.standbys.some((s) => s.node_id === b.to) || ha.role !== 'primary') return err(409, 'conflict', 'That PC is not a connected standby.');
			logEvent('ha.switchover', me.name, `handing over to ${hostOf(b.to)}`);
			failover(0, b.to);
			return json({ to: nodes.get(b.to)?.addr ?? b.to }, 202);
		}
	if (p === '/api/v1/nodes' && m === 'GET') return json({ nodes: [...nodes.values()] });
	const nm = /^\/api\/v1\/nodes\/([^/]+)(\/place)?$/.exec(p);
	if (nm && m === 'GET' && !nm[2]) {
		const n = nodes.get(nm[1]);
		return n ? json({ node: n }) : err(404, 'not_found', 'No such PC.');
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
		return json({ power: powerCfg, asleep: [...nodes.values()].filter((n) => n.power === 'asleep').map((n) => n.node_id) });
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
		const b = await body();
		if (!['sha', 'cmd', 'synth', 'list'].includes(b.kind) || !(b.n > 0)) return err(400, 'invalid_request', 'kind and a positive n are required.');
		if ((b.kind === 'cmd' || b.kind === 'list') && (d = need('operator'))) return d;
		if (b.kind === 'list' && !(Array.isArray(b.items) && b.items.length)) return err(400, 'invalid_request', 'A list job needs items.');
		if (!b[b.kind === 'synth' ? 'synth_ns' : b.kind === 'list' ? 'cmd' : b.kind]) return err(400, 'invalid_request', `Give the ${b.kind} field.`);
		if (b.selector) { const f = parseSel(String(b.selector)); if (typeof f === 'string') return err(400, 'invalid_request', f); }
		const pr = ['urgent', 'high', 'normal', 'low', 'background'].indexOf(b.priority ?? 'normal');
		if (pr < 0) return err(400, 'invalid_request', 'priority must be urgent, high, normal, low or background.');
		const j = mkJob({ kind: b.kind, total: b.n, from: b.from ?? 1, chunk: b.chunk ?? 0, name: b.name ?? '', submitter: me.name, selector: String(b.selector ?? '').trim(), priority: pr, policy: b.policy ?? 'adaptive', reducer: b.reducer ?? '', items: b.items });
		if (!running()) { j.state = 'running'; j.started_ms = now(); }
		logEvent('job.submit', me.name, `job ${j.id} submitted`);
		for (const s of subs) s('job', j);
		return json({ job: j });
	}
	const jm = /^\/api\/v1\/jobs\/(\d+)(\/result|\/cancel)?$/.exec(p);
	if (jm) {
		const j = jobs.find((x) => x.id === Number(jm[1]));
		if (!j) return err(404, 'not_found', `Job ${jm[1]} does not exist.`);
		if (!jm[2]) return json({ job: j, parts: Math.floor(j.done / (j.chunk || 1_000_000)) });
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
	return err(404, 'not_found', `No route ${m} ${p}.`);
}

Bun.serve({ port: PORT, hostname: '127.0.0.1', idleTimeout: 0, fetch: handle });
console.log(`mock ck head on http://127.0.0.1:${PORT} (auth: ${MODE})`);
