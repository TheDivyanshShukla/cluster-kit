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
	cap: z.number().int().nullable().default(null)
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
	eta_ms: z.number().nullable().default(null)
});
export const JobList = z.object({ jobs: z.array(JobRow), next: z.number().nullable() });
export const JobOne = z.object({ job: JobRow, parts: z.number().optional() });

export const Cluster = z.object({ now_ms: z.number(), workers: z.array(Worker), job: JobRow.nullable(), jobs: z.array(JobRow).default([]) });

export const EventRow = z.object({ id: z.number().int(), ts_ms: z.number(), kind: z.string(), actor: z.string(), detail: z.string() });
export const EventList = z.object({ events: z.array(EventRow), next: z.number().nullable() });

export const UserList = z.object({ users: z.array(z.object({ name: z.string(), role: Role, created_ms: z.number() })) });

export const NodeState = z.enum(['pending', 'active', 'cordoned', 'draining', 'maintenance', 'retired']);
export const NodeRow = z.object({
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
	power: z.enum(['awake', 'asleep', 'waking']).catch('awake')
});
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
	dispatching: z.boolean()
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
export const PowerReply = z.object({ power: PowerSettings, asleep: z.array(z.string()).default([]) });
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
