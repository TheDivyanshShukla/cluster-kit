// Typed fetch wrapper for the ck head (docs/API.md). Throws readable errors; never returns unparsed JSON.
// Cookies are same-origin: the session cookie rides along without any option.
import { z } from 'zod';
import { LogList } from './logs';
import {
	SimReply,
	Alert,
	AlertList,
	BreakdownReply,
	DayReportReply,
	ExperimentList,
	ExperimentOne,
	ApiErrorBody,
	CapList,
	CapRow,
	Cluster,
	CandidateList,
	ConfigChanges,
	Estimate,
	EventList,
	HaStatus,
	HaSettingsReply,
	PowerReply,
	SwitchReply,
	JobList,
	JobOne,
	JobRow,
	JobHints,
	JoinCode,
	LabList,
	LabOne,
	Me,
	NewJob,
	NodeList,
	NodeOne,
	NodesChanged,
	TunableList,
	PoolList,
	PoolRow,
	ReserveList,
	ReservationList,
	ReservationRow,
	ReserveRow,
	SelectPreview,
	WindowList,
	WindowRow,
	type NodeState,
	type PowerSettings,
	type HaSettings,
	Stats,
	User,
	UserList,
	JobFiles,
	Net,
	TemplateList,
	AdviceList,
	AdviceHistory,
	AdviceReplyList,
	AdviceApplyReply,
	AdvicePreviewReply,
	AdviceSettingsReply,
	type AdviceMode,
	type Role
} from './schema';

export class ApiError extends Error {
	constructor(
		message: string,
		readonly status = 0,
		readonly code = ''
	) {
		super(message);
	}
}

// JobRow and NewJob strip the wave 10b fields; these keep them.
const JobOneH = z.object({ job: JobRow.extend(JobHints.shape), parts: z.number().optional() });
const TokenReply = z.object({ token: z.string() });
const UserReply = z.object({ user: User });
const PoolOne = z.object({ pool: PoolRow });
const CapOne = z.object({ cap: CapRow });
const ReserveOne = z.object({ reserve: ReserveRow });
const WindowOne = z.object({ window: WindowRow });
const AlertOne = z.object({ alert: Alert });
const ReservationOne = z.object({ reservation: ReservationRow });

let onUnauthorized: (() => void) | null = null;
/** The session store registers itself here so any 401 sends the user back to the login form. */
export function setUnauthorizedHandler(fn: (() => void) | null) {
	onUnauthorized = fn;
}

const fallback: Record<number, string> = {
	400: 'The head rejected that request.',
	401: 'You are signed out. Sign in again.',
	403: 'Your role does not allow that.',
	404: 'Not found.',
	409: 'That conflicts with the current state.',
	429: 'Too many attempts. Wait a few minutes and try again.',
	503: 'The head is not ready yet.'
};

async function send(method: string, path: string, body?: unknown, auth = true): Promise<Response> {
	let r: Response;
	try {
		r = await fetch(path, {
			method,
			credentials: 'same-origin',
			headers: { accept: 'application/json', ...(body !== undefined ? { 'content-type': 'application/json' } : {}) },
			body: body !== undefined ? JSON.stringify(body) : undefined
		});
	} catch {
		throw new ApiError('Cannot reach the head. Is `ck head` running?');
	}
	if (r.ok) return r;
	const parsed = ApiErrorBody.safeParse(await r.json().catch(() => null));
	if (r.status === 401 && auth) onUnauthorized?.();
	throw new ApiError(
		parsed.success ? parsed.data.error.message : (fallback[r.status] ?? `The head answered ${r.status} for ${path}.`),
		r.status,
		parsed.success ? parsed.data.error.code : ''
	);
}

async function call<T extends z.ZodType>(method: string, path: string, schema: T, body?: unknown, auth = true): Promise<z.infer<T>> {
	const r = await send(method, path, body, auth);
	const parsed = schema.safeParse(await r.json().catch(() => null));
	if (!parsed.success) throw new ApiError(`The head sent an unexpected answer for ${path}.`, r.status);
	return parsed.data;
}

const page = (limit?: number, before?: number | null) => {
	const q = new URLSearchParams();
	if (limit) q.set('limit', String(limit));
	if (before != null) q.set('before', String(before));
	const s = q.toString();
	return s ? `?${s}` : '';
};

export const api = {
	// auth
	me: () => call('GET', '/api/v1/me', Me, undefined, false),
	bootstrap: (name: string, password: string) => call('POST', '/api/v1/bootstrap', UserReply, { name, password }, false),
	login: (name: string, password: string) => call('POST', '/api/v1/login', UserReply, { name, password }, false),
	logout: async () => void (await send('POST', '/api/v1/logout', undefined, false)),
	createToken: () => call('POST', '/api/v1/tokens', TokenReply),
	users: () => call('GET', '/api/v1/users', UserList),
	createUser: (u: { name: string; password: string; role: Role }) => call('POST', '/api/v1/users', UserReply, u),
	// cluster
	cluster: () => call('GET', '/api/v1/cluster', Cluster),
	ha: () => call('GET', '/api/v1/ha', HaStatus),
	/** HA-07: make this standby the head; the current head becomes its standby. 409 = not a connected standby. */
	switchover: (to: string) => call('POST', '/api/v1/ha/switchover', SwitchReply, { to }),
	nodes: () => call('GET', '/api/v1/nodes', NodeList),
	/** NET-06: sleep needs a connected PC, wake needs a known MAC (409 otherwise). */
	/** POST /api/v1/sim (researcher): run a `ck sim` scenario, 409 busy while another runs */
	sim: (scenario: unknown) => call('POST', '/api/v1/sim', SimReply, scenario),
	powerNodes: (node_ids: string[], action: 'sleep' | 'wake') => call('POST', '/api/v1/nodes/power', NodesChanged, { node_ids, action }),
	power: () => call('GET', '/api/v1/power', PowerReply),
	setPower: (p: PowerSettings) => call('PUT', '/api/v1/power', PowerReply, p),
	candidates: () => call('GET', '/api/v1/ha/candidates', CandidateList),
	tunables: () => call('GET', '/api/v1/tunables', TunableList),
	/** key -> number, or null to go back to the default. All or nothing. */
	setTunables: (patch: Record<string, number | null>) => call('PUT', '/api/v1/tunables', TunableList, patch),
	haSettings: () => call('GET', '/api/v1/ha/settings', HaSettingsReply),
	setHaSettings: (s: HaSettings) => call('PUT', '/api/v1/ha/settings', HaSettingsReply, s),
	setNodeState: (node_ids: string[], state: NodeState, reason = '') =>
		call('POST', '/api/v1/nodes/state', NodesChanged, { node_ids, state, ...(reason ? { reason } : {}) }),
	/** lab null unplaces the PC. 409 means the seat is taken. */
	place: (id: string, spot: { lab_id: number; x: number; y: number } | { lab_id: null }) =>
		call('PUT', `/api/v1/nodes/${encodeURIComponent(id)}/place`, NodeOne, spot),
	labs: () => call('GET', '/api/v1/labs', LabList),
	createLab: (name: string, w: number, h: number) => call('POST', '/api/v1/labs', LabOne, { name, w, h }),
	patchLab: (id: number, p: { name?: string; w?: number; h?: number }) => call('PATCH', `/api/v1/labs/${id}`, LabOne, p),
	deleteLab: async (id: number) => void (await send('DELETE', `/api/v1/labs/${id}`)),
	createJoinCode: (b: { uses: number; ttl_min: number; auto_approve: boolean }) => call('POST', '/api/v1/join-codes', JoinCode, b),
	// organisation: labels, pools, caps, reserves, selectors, config
	select: (q: string) => call('GET', `/api/v1/select?q=${encodeURIComponent(q)}`, SelectPreview),
	/** value null removes the label */
	setLabels: (id: string, labels: Record<string, string | null>) =>
		call('PUT', `/api/v1/nodes/${encodeURIComponent(id)}/labels`, NodeOne, labels),
	pools: () => call('GET', '/api/v1/pools', PoolList),
	putPool: (name: string, members: string[], note = '') => call('PUT', `/api/v1/pools/${encodeURIComponent(name)}`, PoolOne, { members, note }),
	deletePool: async (name: string) => void (await send('DELETE', `/api/v1/pools/${encodeURIComponent(name)}`)),
	caps: () => call('GET', '/api/v1/caps', CapList),
	addCap: (c: { selector: string; max_threads: number; note?: string }) => call('POST', '/api/v1/caps', CapOne, c),
	deleteCap: async (id: number) => void (await send('DELETE', `/api/v1/caps/${id}`)),
	reserves: () => call('GET', '/api/v1/reserves', ReserveList),
	addReserve: (r: { selector: string; keep_n?: number; keep_pct?: number; note?: string }) => call('POST', '/api/v1/reserves', ReserveOne, r),
	deleteReserve: async (id: number) => void (await send('DELETE', `/api/v1/reserves/${id}`)),
	configText: async () => (await send('GET', '/api/v1/config')).text(),
	configPreview: (cfg: unknown) => call('POST', '/api/v1/config?dry_run=1', ConfigChanges, cfg),
	configApply: async (cfg: unknown) => void (await send('POST', '/api/v1/config', cfg)),
	// jobs
	jobs: (limit = 50, before?: number | null) => call('GET', `/api/v1/jobs${page(limit, before)}`, JobList),
	job: (id: number) => call('GET', `/api/v1/jobs/${id}`, JobOneH),
	jobResult: async (id: number, reducer?: string) =>
		(await send('GET', `/api/v1/jobs/${id}/result${reducer !== undefined ? `?reducer=${encodeURIComponent(reducer)}` : ''}`)).text(),
	estimate: (n: number, kind: string, selector: string) =>
		call('GET', `/api/v1/estimate?${new URLSearchParams({ n: String(n), kind, selector })}`, Estimate),
	// time rules
	windows: () => call('GET', '/api/v1/windows', WindowList),
	addWindow: (w: Record<string, unknown>) => call('POST', '/api/v1/windows', WindowOne, w),
	deleteWindow: async (id: number) => void (await send('DELETE', `/api/v1/windows/${id}`)),
	reservations: () => call('GET', '/api/v1/reservations', ReservationList),
	addReservation: (r: { selector: string; start_ms: number; end_ms: number; note?: string }) => call('POST', '/api/v1/reservations', ReservationOne, r),
	deleteReservation: async (id: number) => void (await send('DELETE', `/api/v1/reservations/${id}`)),
	createJob: (j: NewJob) => call('POST', '/api/v1/jobs', JobOne, (NewJob.parse(j), j)),
	cancelJob: (id: number) => call('POST', `/api/v1/jobs/${id}/cancel`, JobOne),
	// wave 15
	templates: () => call('GET', '/api/v1/templates', TemplateList),
	saveTemplate: async (name: string, spec: unknown, note = '') => void (await send('POST', '/api/v1/templates', { name, spec, note })),
	deleteTemplate: async (name: string) => void (await send('DELETE', `/api/v1/templates/${encodeURIComponent(name)}`)),
	/** the stored spec with these fields laid over it */
	runTemplate: (name: string, overrides: Record<string, unknown> = {}) => call('POST', '/api/v1/jobs', JobOne, { template: name, ...overrides }),
	jobFiles: (id: number) => call('GET', `/api/v1/jobs/${id}/files`, JobFiles),
	identify: (nodes: string[], secs = 30) => send('POST', '/api/v1/nodes/identify', { nodes, secs }).then(() => undefined),
	setQuota: (name: string, q: { max_pcs: number; max_running: number }) => send('PUT', `/api/v1/users/${encodeURIComponent(name)}/quota`, q).then(() => undefined),
	net: () => call('GET', '/api/v1/net', Net),
	/** operator; burst = every PC at once, to learn the head uplink */
	netProbe: (node_ids: string[] = [], burst = false) => send('POST', '/api/v1/net/probe', { node_ids, burst }).then(() => undefined),
	// events
	events: (limit = 50, before?: number | null) => call('GET', `/api/v1/events${page(limit, before)}`, EventList),
	/** operator role; newest lines of the head's log files, filtered by the head */
	logs: (q: { since_ms?: number; kind?: string; level?: string; limit?: number }) => {
		const p = new URLSearchParams();
		for (const [k, v] of Object.entries(q)) if (v !== undefined && v !== '') p.set(k, String(v));
		return call('GET', `/api/v1/logs?${p}`, LogList);
	},
	// research (wave 12)
	breakdown: (id: number) => call('GET', `/api/v1/jobs/${id}/breakdown`, BreakdownReply),
	alerts: (activeOnly = false) => call('GET', `/api/v1/alerts${activeOnly ? '?active=1' : ''}`, AlertList),
	ackAlert: (id: number) => call('POST', `/api/v1/alerts/${id}/ack`, AlertOne),
	experiments: () => call('GET', '/api/v1/experiments', ExperimentList),
	experiment: (id: number) => call('GET', `/api/v1/experiments/${id}`, ExperimentOne),
	report: (day: string) => call('GET', `/api/v1/report?day=${encodeURIComponent(day)}`, DayReportReply),
	// advisor
	advice: () => call('GET', '/api/v1/advice', AdviceList),
	adviceHistory: () => call('GET', '/api/v1/advice/history', AdviceHistory),
	setAdvice: (b: { enabled?: boolean; policy?: Record<string, AdviceMode | null> }) => call('PUT', '/api/v1/advice/settings', AdviceSettingsReply, b),
	advicePreview: (id: string, option?: string) => call('POST', `/api/v1/advice/${encodeURIComponent(id)}/preview`, AdvicePreviewReply, option ? { option } : {}),
	adviceApply: (id: string, option?: string) => call('POST', `/api/v1/advice/${encodeURIComponent(id)}/apply`, AdviceApplyReply, option ? { option } : {}),
	adviceUndo: (id: string) => call('POST', `/api/v1/advice/${encodeURIComponent(id)}/undo`, AdviceReplyList, {}),
	adviceDismiss: (id: string, snooze_h = 24) => call('POST', `/api/v1/advice/${encodeURIComponent(id)}/dismiss`, AdviceReplyList, { snooze_h }),
	// legacy
	stats: (detail = false) => call('GET', detail ? '/stats?detail=1' : '/stats', Stats)
};
