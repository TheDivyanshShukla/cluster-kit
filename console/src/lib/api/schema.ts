// Contract: shapes the ck head returns. Every response is parsed here before the UI sees it.
// Source of truth on the Rust side: rust/src/dash.rs (`/stats`). Lead-owned; change both sides together.
import { z } from 'zod';

export const Inventory = z.object({
	cpu_model: z.string(),
	cores_physical: z.number().int(),
	cores_logical: z.number().int(),
	ram_bytes: z.number(),
	os: z.string(),
	kernel: z.string(),
	arch: z.string(),
	ck_version: z.string()
});

export const Worker = z.object({
	id: z.number().int(),
	addr: z.string(),
	threads: z.number().int(),
	chunks: z.number(),
	numbers: z.number(),
	secs: z.number(),
	rate: z.number(),
	busy: z.boolean(),
	cpu: z.number(),
	mem_used: z.number(),
	mem_total: z.number(),
	version: z.number().int().default(1),
	node_id: z.string().nullable().default(null),
	host: z.string().nullable().default(null),
	virtual: z.boolean().default(false),
	inv: Inventory.nullable().default(null),
	presence: z.enum(['busy', 'idle']).catch('idle'),
	blocked: z.boolean().default(false),
	cap: z.number().int().nullable().default(null),
	/** set on a lab head: the PCs it serves */
	lab_pcs: z.array(z.object({ node_id: z.string().nullable() })).optional()
});

export const Job = z.object({
	id: z.number().int(),
	kind: z.string(),
	done: z.number(),
	total: z.number(),
	secs: z.number()
});

export const Stats = z.object({
	now: z.number(),
	job: Job.nullable(),
	last: z.string().nullable(),
	workers: z.array(Worker),
	events: z.array(z.tuple([z.number(), z.number(), z.number()])).optional()
});

// ---------------------------------------------------------------- API v1 (docs/API.md)

export const Role = z.enum(['viewer', 'researcher', 'operator', 'admin']);
export const User = z.object({ name: z.string(), role: Role });
export const Me = z.object({ user: User.nullable(), mode: z.enum(['open', 'required']), needs_bootstrap: z.boolean() });

export const JobState = z.enum(['queued', 'running', 'done', 'cancelled', 'failed']);
export const JobRow = z.object({
	id: z.number().int(),
	name: z.string(),
	kind: z.enum(['sha', 'cmd', 'synth', 'list']),
	from: z.number(),
	end: z.number(),
	chunk: z.number(),
	total: z.number(),
	done: z.number(),
	state: JobState,
	submitter: z.string(),
	created_ms: z.number(),
	started_ms: z.number().nullable(),
	finished_ms: z.number().nullable(),
	error: z.string().nullable(),
	selector: z.string().default(''),
	priority: z.number().int().min(0).max(4).default(2),
	policy: z.string().default(''),
	reducer: z.string().default(''),
	eta_ms: z.number().nullable().default(null),
	/** wave 15: why a queued job waits, and how many output files it produced */
	wait_reason: z.enum(['quota', 'priority', 'no_pcs']).nullable().catch(null).default(null),
	files: z.number().int().default(0)
});
export const JobList = z.object({ jobs: z.array(JobRow), next: z.number().nullable() });
export const JobOne = z.object({ job: JobRow, parts: z.number().optional() });

export const Cluster = z.object({ now_ms: z.number(), workers: z.array(Worker), job: JobRow.nullable(), jobs: z.array(JobRow).default([]) });

export const EventRow = z.object({ id: z.number().int(), ts_ms: z.number(), kind: z.string(), actor: z.string(), detail: z.string() });
export const EventList = z.object({ events: z.array(EventRow), next: z.number().nullable() });

/** 0 = no limit */
export const Quota = z.object({ max_pcs: z.number().int().default(0), max_running: z.number().int().default(0) });
export type Quota = z.infer<typeof Quota>;
export const UserList = z.object({ users: z.array(z.object({ name: z.string(), role: Role, created_ms: z.number(), quota: Quota.default({ max_pcs: 0, max_running: 0 }) })) });

export const NodeState = z.enum(['pending', 'active', 'cordoned', 'draining', 'maintenance', 'retired']);
/** contract: node `power` may be a plain state or {state, since_ms}; either way NodeRow gets `power` + `power_since_ms` */
const normNode = (v: unknown) => {
	const n = v as Record<string, unknown> | null;
	const p = n?.power;
	if (!n || typeof p !== 'object' || p === null) return v;
	const o = p as Record<string, unknown>;
	return { ...n, power: o.state ?? 'awake', power_since_ms: o.since_ms ?? null };
};
export const NodeRow = z.preprocess(normNode, z.object({
	node_id: z.string(),
	host: z.string(),
	addr: z.string(),
	inventory: Inventory.partial().nullable().catch(null),
	first_seen_ms: z.number(),
	last_seen_ms: z.number(),
	state: NodeState.catch('active'),
	state_reason: z.string().default(''),
	state_by: z.string().default(''),
	lab_id: z.number().int().nullable().default(null),
	x: z.number().int().nullable().default(null),
	y: z.number().int().nullable().default(null),
	online: z.boolean().default(false),
	labels: z.record(z.string(), z.string()).default({}),
	pools: z.array(z.string()).default([]),
	class: z.string().default(''),
	calib_mhs: z.number().nullable().default(null),
	presence: z.enum(['busy', 'idle']).catch('idle'),
	/** NET-06: asleep = the head put it to sleep; waking = magic packet sent, not back yet */
	power: z.enum(['awake', 'asleep', 'waking']).catch('awake'),
	/** when the PC entered that power state (epoch ms); null if the head does not know */
	power_since_ms: z.number().nullish(),
	/** FLT-07: the PC is showing its "it is me" banner until this epoch ms; null when off */
	identify_until_ms: z.number().nullish(),
	/** lab head (name of the lab) whose connection this PC works through; null = straight to the root */
	via_lab: z.string().nullish().catch(null)
}));
export const NodeOne = z.object({ node: NodeRow });
export const NodesChanged = z.object({ nodes: z.array(NodeRow) });
export type NodeState = z.infer<typeof NodeState>;

export const LabRow = z.object({ id: z.number().int(), name: z.string(), w: z.number().int(), h: z.number().int(), created_ms: z.number() });
export const LabList = z.object({ labs: z.array(LabRow) });
export const LabOne = z.object({ lab: LabRow });
export type LabRow = z.infer<typeof LabRow>;

export const JoinCode = z.object({ code: z.string(), expires_ms: z.number(), uses: z.number().int(), auto_approve: z.boolean() });
export type JoinCode = z.infer<typeof JoinCode>;
export const NodeList = z.object({ nodes: z.array(NodeRow) });
export type NodeRow = z.infer<typeof NodeRow>;

export const ApiErrorBody =z.object({ error: z.object({ code: z.string(), message: z.string() }) });

/** POST /api/v1/jobs body. */
export const NewJob = z
	.object({
		name: z.string().max(120).optional(),
		kind: z.enum(['sha', 'cmd', 'synth', 'list']),
		from: z.number().int().min(0).default(1),
		n: z.number().int().positive(),
		chunk: z.number().int().min(0).default(0),
		sha: z.string().regex(/^[0-9a-f]{1,64}$/i).optional(),
		cmd: z.string().min(1).optional(),
		synth_ns: z.number().int().positive().optional(),
		selector: z.string().max(400).optional(),
		priority: z.enum(['urgent', 'high', 'normal', 'low', 'background']).optional(),
		policy: z.enum(['adaptive', 'guided', 'fixed']).optional(),
		reducer: z.string().max(20).optional(),
		items: z.array(z.string().max(4096)).max(1_000_000).optional()
	})
	.refine((j) => (j.kind === 'sha' ? !!j.sha : j.kind === 'cmd' ? !!j.cmd : j.kind === 'list' ? !!j.cmd && !!j.items?.length : !!j.synth_ns), {
		message: 'Give the field that matches the kind: sha, cmd, synth_ns, or cmd + items for a list job.'
	});

export type Role = z.infer<typeof Role>;
export type User = z.infer<typeof User>;
export type Me = z.infer<typeof Me>;
export type JobState = z.infer<typeof JobState>;
export type JobRow = z.infer<typeof JobRow>;
export type Cluster = z.infer<typeof Cluster>;
export type EventRow = z.infer<typeof EventRow>;
export type NewJob = z.input<typeof NewJob>;

export type Inventory = z.infer<typeof Inventory>;
export type Worker = z.infer<typeof Worker>;
export type Job = z.infer<typeof Job>;
export type Stats = z.infer<typeof Stats>;

// ---------------------------------------------------------------- M2 organisation
export const PRIORITIES = ['urgent', 'high', 'normal', 'low', 'background'] as const;
export const SelectPreview = z.object({ count: z.number().int(), node_ids: z.array(z.string()), error: z.string().nullable() });
export const PoolRow = z.object({ name: z.string(), members: z.array(z.string()), note: z.string().default('') });
export const PoolList = z.object({ pools: z.array(PoolRow) });
export const CapRow = z.object({ id: z.number().int(), selector: z.string(), max_threads: z.number().int().nullable(), note: z.string().default('') });
export const CapList = z.object({ caps: z.array(CapRow) });
export const ReserveRow = z.object({
	id: z.number().int(),
	selector: z.string(),
	keep_n: z.number().int(),
	keep_pct: z.number().int(),
	note: z.string().default(''),
	held: z.array(z.string()).default([])
});
export const ReserveList = z.object({ reserves: z.array(ReserveRow) });
export const ConfigChanges = z.object({ changes: z.array(z.string()) });
export type SelectPreview = z.infer<typeof SelectPreview>;
export type PoolRow = z.infer<typeof PoolRow>;
export type CapRow = z.infer<typeof CapRow>;
export type ReserveRow = z.infer<typeof ReserveRow>;

// ---------------------------------------------------------------- M3 time rules, estimates
export const REDUCERS = ['', 'concat', 'lines', 'sum', 'count', 'sort'] as const; // plus top:N
export const WindowRow = z.object({
	id: z.number().int(),
	selector: z.string(),
	days: z.string(),
	start: z.string(),
	end: z.string(),
	action: z.enum(['block', 'cap']),
	cap_threads: z.number().int().nullable().default(null),
	note: z.string().default(''),
	active_now: z.boolean().default(false)
});
export const WindowList = z.object({ windows: z.array(WindowRow) });
export const ReservationRow = z.object({
	id: z.number().int(),
	selector: z.string(),
	start_ms: z.number(),
	end_ms: z.number(),
	note: z.string().default(''),
	active_now: z.boolean().default(false)
});
export const ReservationList = z.object({ reservations: z.array(ReservationRow) });
export const Estimate = z.object({ matching_pcs: z.number().int(), fleet_rate: z.number(), eta_secs: z.number().nullable() });
export type WindowRow = z.infer<typeof WindowRow>;
export type ReservationRow = z.infer<typeof ReservationRow>;
export type Estimate = z.infer<typeof Estimate>;

// ---------------------------------------------------------------- M4 high availability
export const HaStatus = z.object({
	role: z.enum(['primary', 'standby', 'candidate']),
	epoch: z.number().int(),
	last_seq: z.number().int(),
	durable_seq: z.number().int(),
	standbys: z.array(z.object({ node_id: z.string(), acked_seq: z.number().int(), lag: z.number().int() })),
	membership: z.number().int(),
	connected: z.number().int(),
	dispatching: z.boolean(),
	/** who is primary now and since when (epoch-start ms) */
	head: z.object({ node_id: z.string(), addr: z.string().default(''), since_ms: z.number().nullable().default(null) }).nullable().default(null)
});
export type HaStatus = z.infer<typeof HaStatus>;

/** POST /api/v1/ha/switchover reply (HA-07): the address of the standby that takes over. */
export const SwitchReply = z.object({ to: z.string() });

/** GET/PUT /api/v1/power (NET-06) */
export const PowerSettings = z.object({
	mode: z.enum(['off', 'eco']).catch('off'),
	idle_min: z.number().int(),
	keep_awake: z.number().int(),
	wake_on_demand: z.boolean()
});
export const LabPower = z.object({ lab: z.string(), watts: z.number(), awake: z.number().int().default(0), asleep: z.number().int().default(0), waking: z.number().int().default(0) });
export type LabPower = z.infer<typeof LabPower>;
export const PowerReply = z.object({ power: PowerSettings, asleep: z.array(z.string()).default([]), labs: z.array(LabPower).default([]) });

/** SSE `changed`: a list was written, refetch it. SSE `log`: batched log lines. */
export const WHAT = ['templates', 'reserves', 'bookings', 'timetable', 'candidates', 'pools', 'caps', 'labels', 'windows'] as const;
export type What = (typeof WHAT)[number];
export const Changed = z.object({ what: z.enum(WHAT) });
export const LogBatch = z.object({ lines: z.array(z.looseObject({ ts_ms: z.number(), level: z.enum(['error', 'warn', 'info', 'debug']), kind: z.string() })) });

/** POST /api/v1/sim reply: the JSON `ck sim` writes (rust/src/sim.rs); its shape is normalised by lib/sim.ts */
export const SimReply = z.record(z.string(), z.unknown());
/** GET/PUT /api/v1/ha/settings (HA-10) */
export const HaSettings = z.object({ target_standbys: z.number().int(), selector: z.string() });
export const HaSettingsReply = z.object({ settings: HaSettings });
export type PowerSettings = z.infer<typeof PowerSettings>;
export type HaSettings = z.infer<typeof HaSettings>;

/** GET/PUT /api/v1/tunables (wave 9: every value adjustable) */
export const Tunable = z.object({
	key: z.string(),
	group: z.string(),
	value: z.number(),
	default: z.number(),
	min: z.number(),
	max: z.number(),
	unit: z.string(),
	scope: z.enum(['head', 'agent', 'both']).catch('head'),
	help: z.string(),
	source: z.enum(['default', 'db', 'env', 'head']).catch('default')
});
export const TunableList = z.object({ tunables: z.array(Tunable) });
export type Tunable = z.infer<typeof Tunable>;
/** GET /api/v1/ha/candidates (HA-10 head choice) */
export const Candidate = z.object({
	node_id: z.string(),
	host: z.string(),
	lab: z.string().nullable().default(null),
	score: z.number(),
	parts: z.object({ speed: z.number(), memory: z.number(), stability: z.number(), free: z.number() }),
	role: z.enum(['head', 'standby', 'candidate']).catch('candidate'),
	eligible: z.boolean(),
	why: z.string().default('')
});
export const CandidateList = z.object({ candidates: z.array(Candidate) });
export type Candidate = z.infer<typeof Candidate>;

/** Wave 10b job fields (also on JobRow when the head sends them) */
export const Placement = z.enum(['spread', 'pack', 'fastest', 'efficient']);
export type Placement = z.infer<typeof Placement>;
export const JobHints = z.object({
	max_pcs: z.number().int().default(0),
	placement: z.union([Placement, z.literal('')]).catch(''),
	mem_mb: z.number().int().default(0)
});

/** Wave 12 research features (docs/API.md "Research features") */
export const Breakdown = z.object({
	job: z.number().int(),
	wall_s: z.number(),
	pc_seconds: z.object({ compute: z.number(), dispatch_wait: z.number(), tail_idle: z.number(), recompute: z.number(), failover_pause: z.number(), other: z.number() }),
	per_pc: z.array(z.object({ node_id: z.string(), host: z.string().default(''), chunks: z.number(), numbers: z.number(), rate: z.number(), busy_s: z.number() })),
	chunk_s: z.object({ p50: z.number(), p90: z.number(), max: z.number() }),
	stragglers: z.array(z.string()).default([]),
	efficiency: z.number()
});
export const BreakdownReply = z.object({ breakdown: Breakdown });
export const Alert = z.object({
	id: z.number().int(),
	kind: z.enum(['slow_pc', 'flaky_pc', 'head_hot', 'standby_behind', 'reserve_at_risk', 'no_standby']).catch('slow_pc'),
	severity: z.enum(['info', 'warn', 'bad']).catch('warn'),
	node_id: z.string().nullable().default(null),
	detail: z.string(),
	since_ms: z.number(),
	last_ms: z.number(),
	active: z.boolean(),
	acked_by: z.string().nullable().default(null)
});
export const AlertList = z.object({ alerts: z.array(Alert) });
export const ExperimentRow = z.object({
	id: z.number().int(), name: z.string(), submitter: z.string(), created_ms: z.number(), configs: z.number().int(),
	repeats: z.number().int(), virtual: z.boolean(), commit: z.string().default(''), pcs_max: z.number().int()
});
export const ExperimentList = z.object({ experiments: z.array(ExperimentRow) });
export const ExperimentOne = z.object({ experiment: ExperimentRow, raw_csv: z.string(), runs_jsonl: z.string(), sim: z.unknown().nullable().default(null) });
export const DayReport = z.object({
	day: z.string(), throughput_peak: z.number(), throughput_mean: z.number(),
	work_done: z.object({ numbers: z.number(), items: z.number(), jobs_done: z.number(), jobs_failed: z.number() }),
	pcs: z.object({ seen: z.number(), max_online: z.number(), mean_online: z.number() }),
	failures: z.object({ pcs_lost: z.number(), recovered_chunks: z.number(), head_failovers: z.number() }),
	energy_kwh: z.number().nullable().default(null), busy_share: z.number(),
	top_jobs: z.array(z.object({ id: z.number(), name: z.string(), pc_hours: z.number() })), alerts: z.number()
});
export const DayReportReply = z.object({ report: DayReport });
export type Breakdown = z.infer<typeof Breakdown>;
export type Alert = z.infer<typeof Alert>;
export type ExperimentRow = z.infer<typeof ExperimentRow>;
export type DayReport = z.infer<typeof DayReport>;

/** Wave 15: templates (JOB-12), job output files (JOB-08), network (UI-07) */
export const Template = z.object({ name: z.string(), spec: z.record(z.string(), z.unknown()), note: z.string().default(''), owner: z.string().default(''), created_ms: z.number() });
export const TemplateList = z.object({ templates: z.array(Template) });
export type Template = z.infer<typeof Template>;
export const JobFile = z.object({ chunk: z.tuple([z.number(), z.number()]), name: z.string(), sha: z.string(), size: z.number() });
export const JobFiles = z.object({ files: z.array(JobFile) });
export type JobFile = z.infer<typeof JobFile>;
export const NetNode = z.object({
	node_id: z.string(), host: z.string().default(''), lab: z.string().default(''),
	net_mbps: z.number().nullish(), rtt_ms: z.number().nullish(), probed_ms: z.number().nullish()
});
export const NetLab = z.object({ lab: z.string(), pcs: z.number().int(), mbps: z.number().nullish() });
export const Net = z.object({
	head_uplink_mbps: z.number().nullish(), head_uplink_assumed: z.boolean().default(false), head_bytes_sent: z.number().default(0),
	nodes: z.array(NetNode), labs: z.array(NetLab)
});
export type Net = z.infer<typeof Net>;
export type NetNode = z.infer<typeof NetNode>;

// Advisor (docs/ADVISOR.md): suggestions with options, apply/undo history, policy.
export const AdviceMode = z.enum(['off', 'suggest', 'auto']);
export const AdviceAction = z.looseObject({ type: z.string() });
export const AdviceOption = z.object({
	id: z.string(),
	title: z.string(),
	action: AdviceAction.nullable().default(null),
	steps: z.array(z.string()).default([]),
	expected_effect: z.string().default(''),
	metric: z.string().nullable().default(null),
	metric_from: z.number().default(0),
	metric_to: z.number().default(0),
	cost: z.string().default(''),
	risk: z.enum(['low', 'med', 'high']).catch('low'),
	reversible: z.boolean().default(true),
	auto_safe: z.boolean().default(false),
	time_to_effect: z.string().default(''),
	confidence: z.string().default(''),
	success_check: z.string().default(''),
	recommended: z.boolean().default(false),
	score: z.number().default(0)
});
export const Suggestion = z.object({
	id: z.string(),
	kind: z.string(),
	severity: z.enum(['info', 'tip', 'warn']).catch('info'),
	title: z.string(),
	why: z.string().default(''),
	options: z.array(AdviceOption).default([]),
	effect: z.string().default(''),
	action: AdviceAction.nullable().default(null),
	since_ms: z.number().default(0),
	policy: z.string().default('suggest'),
	accuracy: z.number().nullable().optional()
});
export const AdviceOutcome = z.object({ observed: z.number().nullish(), predicted_gain: z.number().nullish(), observed_gain: z.number().nullish(), hit: z.boolean().nullish() });
export const AdviceApplied = z.object({
	id: z.string(),
	kind: z.string(),
	option: z.string(),
	action: AdviceAction.nullable().default(null),
	by: z.string().default(''),
	auto: z.boolean().default(false),
	ts_ms: z.number(),
	metric: z.string().nullable().default(null),
	before: z.number().nullish(),
	predicted: z.number().nullish(),
	follow_s: z.number().default(0),
	undone: z.boolean().default(false),
	outcome: AdviceOutcome.nullable().default(null)
});
const Accuracy = z.record(z.string(), z.number()).default({});
export const AdviceList = z.object({
	enabled: z.boolean(),
	policy: z.record(z.string(), AdviceMode.catch('suggest')).default({}),
	suggestions: z.array(Suggestion),
	accuracy: Accuracy
});
export const AdviceSettingsReply = z.object({ enabled: z.boolean(), policy: z.record(z.string(), AdviceMode.catch('suggest')).default({}), suggestions: z.array(Suggestion).default([]) });
export const AdviceHistory = z.object({ history: z.array(AdviceApplied), accuracy: Accuracy });
export const AdviceReplyList = z.object({ suggestions: z.array(Suggestion) });
export const AdviceApplyReply = z.object({ applied: AdviceApplied, suggestions: z.array(Suggestion).default([]) });
export const AdviceChange = z.looseObject({ setting: z.string().optional(), node: z.string().optional(), from: z.unknown().optional(), to: z.unknown().optional() });
export const AdvicePreviewReply = z.object({
	id: z.string(),
	option: AdviceOption,
	preview: z.object({ would_change: z.array(AdviceChange).default([]), valid: z.boolean().default(true), error: z.string().nullish(), doc: z.string().optional() }).nullable(),
	changes_nothing: z.boolean().default(true)
});
export const AdviceEvent = z.object({ suggestions: z.array(Suggestion) });
export type AdviceMode = z.infer<typeof AdviceMode>;
export type Suggestion = z.infer<typeof Suggestion>;
export type AdviceOption = z.infer<typeof AdviceOption>;
export type AdviceApplied = z.infer<typeof AdviceApplied>;
export type AdvicePreviewReply = z.infer<typeof AdvicePreviewReply>;

/** GET /api/v1/hierarchy (docs/HIERARCHY.md): the lab-head tree */
const n0 = z.number().catch(0);
export const HierMode = z.enum(['off', 'manual', 'auto']);
export type HierMode = z.infer<typeof HierMode>;
export const LabHead = z.object({
	node_id: z.string(),
	host: z.string().nullish(),
	addr: z.string().catch(''),
	port: n0,
	connected_s: z.number().nullish(),
	pcs: n0,
	threads: n0,
	/** numbers the root has handed it and still counts as in flight */
	granted_in_flight: n0,
	/** as the lab head sees it: numbers in the leases it holds, and how many of them its PCs finished */
	lab_granted: n0,
	lab_done: n0,
	leases_done: n0,
	lag_s: z.number().nullish(),
	unacked: n0,
	partitioned: z.boolean().catch(false),
	released: z.boolean().catch(false),
	rate: n0,
	root_inflight: z.array(z.object({ job: z.number(), a: z.number(), b: z.number() })).catch([])
});
export type LabHead = z.infer<typeof LabHead>;
export const HierLabState = z.enum(['active', 'partitioned', 'released', 'absent', 'none']).catch('none');
export type HierLabState = z.infer<typeof HierLabState>;
export const HierLab = z.object({
	lab: z.string(),
	pinned: z.string().nullish(),
	drain: z.boolean().catch(false),
	state: HierLabState,
	head: LabHead.nullish(),
	pcs: z.array(z.object({ id: z.string(), host: z.string().catch(''), threads: n0, busy: z.boolean().catch(false) })).catch([]),
	pcs_at_root: n0,
	silent_s: z.number().nullish(),
	orphaned_leases: z.number().nullish()
});
export type HierLab = z.infer<typeof HierLab>;
export const Hierarchy = z.object({
	mode: HierMode.catch('off'),
	active: z.boolean().catch(false),
	paused: z.string().nullish(),
	settings: z.object({ lease_s: n0, lab_head_min_pcs: n0, lab_silence_s: n0, lab_head_standby: z.boolean().catch(false) }),
	root: z.object({ node_id: z.string().catch(''), role: z.string().catch('primary'), epoch: n0, addr: z.string().nullish(), pcs_direct: n0 }),
	standbys: z.array(z.string()).catch([]),
	labs: z.array(HierLab),
	orphans: z.array(z.object({ node_id: z.string(), lab: z.string(), leases: n0, silent_s: n0, expires_in_s: n0 })).catch([]),
	unleasable_jobs: z.array(z.number()).catch([])
});
export type Hierarchy = z.infer<typeof Hierarchy>;
export const HierSettingsPatch = z.object({ mode: HierMode, lease_s: z.number(), lab_head_min_pcs: z.number(), lab_silence_s: z.number() }).partial();
export type HierSettingsPatch = z.infer<typeof HierSettingsPatch>;
