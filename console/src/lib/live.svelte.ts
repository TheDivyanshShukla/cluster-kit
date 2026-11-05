// Live cluster state over SSE (/api/v1/stream). One connection for the whole app.
// If the stream drops, each reconnect attempt also reads GET /api/v1/cluster, so numbers keep
// moving (slower) while the banner says "reconnecting".
import { api, ApiError } from './api/client';
import { runningJobs } from './jobs';
import { failoverStep, noFailover, primaryAddr, type Failover } from './ha';
import { Alert, Changed, Cluster, LogBatch, WHAT, type What, EventRow, HaStatus, JobRow, NodeRow, type HaSettings, type LabPower, type LabRow, type PowerSettings } from './api/schema';

export const live = $state({
	cluster: null as Cluster | null,
	link: 'connecting' as 'connecting' | 'live' | 'reconnecting',
	error: null as string | null,
	/** newest known state of every job seen on the stream or fetched by a page, by id */
	jobs: {} as Record<number, JobRow>,
	/** events seen on the stream, newest first */
	events: [] as EventRow[],
	/** every registered PC by node_id: snapshot at start, then `node` events */
	nodes: {} as Record<string, NodeRow>,
	labs: [] as LabRow[],
	/** node_id -> when its chunk count last went up (tiles flash on this) */
	flash: {} as Record<string, number>,
	/** PCs that dropped off the cluster feed while a job ran: node_id -> {host, since}. Cleared when they return or the job ends. */
	lost: {} as Record<string, { host: string; at: number }>,
	/** GET /api/v1/ha, then `ha` events */
	ha: null as HaStatus | null,
	/** set when the API answers 503 unavailable: this head is a standby; primary is its address (if told) */
	standby503: null as { primary: string | null } | null,
	failover: noFailover as Failover,
	/** HA-10 settings (standbys to keep, selector) and the PCs that selector matches; null until loaded or if this role may not read them */
	haCfg: null as HaSettings | null,
	haCandidates: null as string[] | null,
	/** a planned switchover (Make head) is under way: shown as "Handing over" instead of "Head lost" */
	handover: null as { to: string; at: number } | null,
	/** node_id -> when this console first saw it waking (the API has no wake timestamp) */
	wakeSince: {} as Record<string, number>,
	powerCfg: null as PowerSettings | null,
	/** every alert seen: read at start, then `alert` events (merged by id). Active ones have `active` set. */
	alerts: [] as Alert[],
	/** SSE `changed`: a counter per list that goes up after each write of it; panels refetch when theirs moves */
	rev: Object.fromEntries(WHAT.map((w) => [w, 0])) as Record<What, number>,
	/** SSE `log`: newest log lines first, bounded; the Logs view merges them over its fetched page */
	logTail: [] as Record<string, unknown>[],
	logRev: 0,
	/** GET /api/v1/power `labs`: watts per lab (same model as sim and eco) */
	labPower: [] as LabPower[]
});
const TAIL_MAX = 500;
const putAlert = (a: Alert) => (live.alerts = [a, ...live.alerts.filter((x) => x.id !== a.id)].sort((x, y) => y.last_ms - x.last_ms));

function noteWake(n: NodeRow) {
	if (n.power === 'waking') live.wakeSince[n.node_id] ??= Date.now();
	else delete live.wakeSince[n.node_id];
}

export async function loadHaCfg() {
	try {
		const { settings } = await api.haSettings();
		live.haCfg = settings;
		const q = settings.selector.trim();
		live.haCandidates = q ? (await api.select(q)).node_ids : Object.keys(live.nodes);
	} catch {
		// a role that may not read the settings simply gets no recruit slot
	}
}
export async function loadPowerCfg() {
	try {
		const r = await api.power();
		live.powerCfg = r.power;
		live.labPower = r.labs;
	} catch {
		live.powerCfg = null;
	}
}
let lastEpoch: number | null = null;

/** This console is served by a standby head. */
export const isStandby = () => live.ha?.role === 'standby' || live.standby503 !== null;
let everLive = false;
/** Re-evaluate the failover timer (called on every ha/link change and by the wall's clock tick). */
export function tickFailover(now = Date.now()) {
	if (live.ha && lastEpoch != null && live.ha.epoch > lastEpoch) live.handover = null;
	if (live.handover && now - live.handover.at > 45_000) live.handover = null;
	lastEpoch = live.ha?.epoch ?? lastEpoch;
	const down = live.ha?.role === 'candidate' || (everLive && live.link === 'reconnecting' && !isStandby());
	live.failover = failoverStep(live.failover, down, now);
}

export async function loadHa() {
	try {
		live.ha = await api.ha();
	} catch (e) {
		if (e instanceof ApiError && e.code === 'unavailable') live.standby503 = { primary: primaryAddr(e.message) };
	}
	tickFailover();
}

let prevChunks = new Map<string, number>();

let es: EventSource | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
let fails = 0;

function setCluster(c: Cluster) {
	const seen = new Map<string, number>();
	const t = Date.now();
	for (const w of c.workers) {
		if (!w.node_id) continue;
		seen.set(w.node_id, w.chunks);
		if (prevChunks.has(w.node_id) && w.chunks > prevChunks.get(w.node_id)!) live.flash[w.node_id] = t;
		delete live.lost[w.node_id];
	}
	const run = runningJobs(c);
	if (!run.length) for (const k of Object.keys(live.lost)) delete live.lost[k];
	else for (const id of prevChunks.keys()) if (!seen.has(id) && !live.lost[id] && (live.nodes[id]?.power ?? 'awake') === 'awake') live.lost[id] = { host: live.nodes[id]?.host ?? id, at: t };
	prevChunks = seen;
	live.cluster = c;
	for (const j of run) live.jobs[j.id] = j;
}

function listen<T>(name: string, parse: (v: unknown) => { success: boolean; data?: T }, apply: (v: T) => void) {
	es!.addEventListener(name, (e) => {
		try {
			const p = parse(JSON.parse((e as MessageEvent).data));
			if (p.success) apply(p.data as T);
		} catch {
			// a malformed frame is skipped; the next one repairs the view
		}
	});
}

/** Re-read PCs and labs (labs are not streamed, so editors call this after a change). */
export async function loadFleet() {
	try {
		const [n, l] = await Promise.all([api.nodes(), api.labs()]);
		live.nodes = Object.fromEntries(n.nodes.map((x) => [x.node_id, x]));
		n.nodes.forEach(noteWake);
		live.labs = l.labs;
	} catch (e) {
		live.error = e instanceof ApiError ? e.message : 'Something went wrong reading the head.';
	}
}

async function probe() {
	try {
		setCluster(await api.cluster());
		live.standby503 = null;
		void loadFleet();
		live.error = null;
	} catch (e) {
		if (e instanceof ApiError && e.code === 'unavailable') live.standby503 = { primary: primaryAddr(e.message) };
		else live.error = e instanceof ApiError ? e.message : 'Something went wrong reading the head.';
	}
	void loadHa();
	api.alerts().then((r) => r.alerts.forEach(putAlert)).catch(() => {});
}

function connect() {
	timer = null;
	const s = (es = new EventSource('/api/v1/stream'));
	s.onopen = () => {
		fails = 0;
		live.link = 'live';
		everLive = true;
		live.error = null;
		tickFailover();
	};
	listen('ha', (v) => HaStatus.safeParse(v), (h) => {
		live.ha = h;
		tickFailover();
	});
	listen('cluster', (v) => Cluster.safeParse(v), setCluster);
	listen('job', (v) => JobRow.safeParse(v), (j) => {
		live.jobs[j.id] = j;
	});
	listen('node', (v) => NodeRow.safeParse(v), (n) => {
		live.nodes[n.node_id] = n;
		noteWake(n);
	});
	listen('changed', (v) => Changed.safeParse(v), ({ what }) => {
		live.rev[what]++;
		if (what === 'labels') void loadFleet();
		if (what === 'candidates') void loadHaCfg();
	});
	listen('log', (v) => LogBatch.safeParse(v), ({ lines }) => {
		live.logTail = [...lines.slice().reverse(), ...live.logTail].slice(0, TAIL_MAX);
		live.logRev++;
	});
	listen('alert', (v) => Alert.safeParse(v), putAlert);
	listen('event', (v) => EventRow.safeParse(v), (ev) => {
		live.events = [ev, ...live.events.filter((x) => x.id !== ev.id)].slice(0, 100);
		if (ev.kind.startsWith('power.') && live.powerCfg) void loadPowerCfg();
		if (ev.kind === 'ha.recruit') {
			void loadHa();
			void loadHaCfg();
		}
	});
	s.onerror = () => {
		s.close();
		es = null;
		fails++;
		live.link = 'reconnecting';
		tickFailover();
		void probe();
		timer = setTimeout(connect, Math.min(1000 * 2 ** (fails - 1), 5000));
	};
}

/** Start the stream once; returns the stop function for the layout. */
export function startLive() {
	if (!es && !timer) {
		void probe(); // numbers on first paint, before the stream's first `cluster` frame
		connect();
	}
	return () => {
		es?.close();
		es = null;
		if (timer) clearTimeout(timer);
		timer = null;
		fails = 0;
		live.link = 'connecting';
	};
}
