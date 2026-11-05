// Command palette logic (docs/DESIGN.md section 9, UI-12): query parsing, fuzzy ranking, selector detection and the
// action templates. Pure: the component (CommandPalette.svelte) gathers the entries and runs the actions.
import type { NodeState, Role } from './api/schema';
import { parseSelector } from './canvas/select';

export type Kind = 'action' | 'pc' | 'job' | 'user' | 'lab' | 'pool' | 'set' | 'page' | 'event' | 'log';
/** group order in the results */
export const KIND_ORDER: Kind[] = ['action', 'pc', 'job', 'lab', 'pool', 'user', 'set', 'page', 'event', 'log'];
export const KIND_LABEL: Record<Kind, string> = { action: 'Actions', pc: 'PCs', job: 'Jobs', lab: 'Labs', pool: 'Pools', user: 'Users', set: 'Settings', page: 'Pages', event: 'Recent events', log: 'Log lines' };

export type Run =
	| { type: 'state'; state: NodeState; ids: string[] }
	| { type: 'power'; action: 'sleep' | 'wake'; ids: string[] }
	| { type: 'head'; id: string };
export interface Act {
	run: Run;
	title: string;
	body: string;
	confirm: string;
}
export interface Entry {
	id: string;
	kind: Kind;
	label: string;
	sub?: string;
	/** extra searchable text */
	keys?: string;
	href?: string;
	/** needs at least this role; shown disabled below it */
	need?: Role;
	act?: Act;
}
export interface Parsed {
	/** a type prefix was given: only that group is searched */
	kind: Kind | null;
	text: string;
	/** the whole text is a selector (every term is key=value style) */
	selector: string | null;
}

const PREFIX: Record<string, Kind> = { pc: 'pc', job: 'job', set: 'set', user: 'user', lab: 'lab', pool: 'pool', page: 'page', ev: 'event', event: 'event', log: 'log' };
export function parseQuery(q: string): Parsed {
	const t = q.trimStart();
	if (t.startsWith('>')) return { kind: 'action', text: t.slice(1).trim(), selector: null };
	const m = /^([a-z]+):(?!=)\s*(.*)$/i.exec(t);
	if (m && PREFIX[m[1].toLowerCase()] && !/^\S*=/.test(m[2].split(/\s/)[0] ?? '')) return { kind: PREFIX[m[1].toLowerCase()], text: m[2].trim(), selector: null };
	return { kind: null, text: t.trim(), selector: selectorOf(t) };
}
/** `lab=A state=lost`: every term has an operator and the whole thing parses. A bare word is never a selector. */
export function selectorOf(text: string): string | null {
	const s = text.trim();
	const terms = s.match(/(?:[^\s"]|"[^"]*")+/g) ?? [];
	if (!terms.length || !terms.every((t) => /^[a-z_][a-z0-9_.-]*(!=|>=|<=|=|>|<).+$/i.test(t))) return null;
	return typeof parseSelector(s) === 'string' ? null : s;
}

/**
 * Fuzzy score of `query` against `text`, or -1 when it does not match. Exact > prefix > word start > substring >
 * subsequence; shorter text and earlier hits win ties.
 */
export function fuzzy(text: string, query: string): number {
	const t = text.toLowerCase(), q = query.toLowerCase().trim();
	if (!q) return 1;
	if (t === q) return 1000;
	if (t.startsWith(q)) return 800 - Math.min(t.length - q.length, 100);
	const words = q.split(/\s+/);
	if (words.length > 1) {
		const parts = words.map((w) => fuzzy(text, w));
		return parts.some((p) => p < 0) ? -1 : Math.min(...parts) - 10;
	}
	const i = t.indexOf(q);
	if (i >= 0) return (/[\s\-_./:=]/.test(t[i - 1] ?? ' ') ? 600 : 400) - Math.min(i, 100) - Math.min(t.length - q.length, 100) / 10;
	// subsequence, rewarding consecutive hits and word starts
	let at = 0, score = 100, run = 0;
	for (const c of q) {
		const j = t.indexOf(c, at);
		if (j < 0) return -1;
		run = j === at ? run + 1 : 0;
		score += run * 4 + (/[\s\-_./]/.test(t[j - 1] ?? ' ') ? 6 : 0) - Math.min(j - at, 10);
		at = j + 1;
	}
	return Math.max(1, Math.min(score, 350) - Math.min(t.length, 60) / 4);
}

export interface Hit {
	e: Entry;
	score: number;
}
const RECENT_BOOST = 300;
/** Entries that match, best first inside each group; groups follow KIND_ORDER. `limit` per group (more when a prefix narrows it). */
export function rank(entries: Entry[], p: Parsed, recent: string[] = [], limit = 5): Hit[] {
	const out: Hit[] = [];
	const per = new Map<Kind, Hit[]>();
	for (const e of entries) {
		if (p.kind && e.kind !== p.kind) continue;
		let s = p.text ? Math.max(fuzzy(e.label, p.text), fuzzy(`${e.label} ${e.sub ?? ''} ${e.keys ?? ''}`, p.text) - 40) : 1;
		if (s < 0) continue;
		const r = recent.indexOf(e.id);
		if (r >= 0) s += RECENT_BOOST - r * 10;
		(per.get(e.kind) ?? per.set(e.kind, []).get(e.kind)!).push({ e, score: s });
	}
	for (const k of KIND_ORDER) {
		const list = (per.get(k) ?? []).sort((a, b) => b.score - a.score || a.e.label.localeCompare(b.e.label, 'en', { numeric: true }));
		out.push(...list.slice(0, p.kind ? 50 : limit));
	}
	return out;
}

// ---- actions: "drain pc-12", "sleep lab B", "make pc-18 head", "new job", "set lost_after_s"
export interface Ctx {
	nodes: { node_id: string; host: string; lab_id: number | null }[];
	labs: { id: number; name: string }[];
}
const VERBS: { verb: RegExp; word: string; state?: NodeState; power?: 'sleep' | 'wake'; need: Role; body: string }[] = [
	{ verb: /^drain\b/, word: 'Drain', state: 'draining', need: 'operator', body: 'Each PC finishes the tasks it holds, then stops taking new ones.' },
	{ verb: /^cordon\b/, word: 'Cordon', state: 'cordoned', need: 'operator', body: 'The PCs take no new tasks until someone activates them again.' },
	{ verb: /^(activate|uncordon|resume)\b/, word: 'Activate', state: 'active', need: 'operator', body: 'The PCs take work again.' },
	{ verb: /^maintenance\b/, word: 'Start maintenance on', state: 'maintenance', need: 'operator', body: 'The PCs take no new tasks until maintenance ends.' },
	{ verb: /^sleep\b/, word: 'Sleep', power: 'sleep', need: 'operator', body: 'Each PC suspends now. Anything running on it stops, and it comes back only when woken (Wake-on-LAN) or by its power button.' },
	{ verb: /^wake\b/, word: 'Wake', power: 'wake', need: 'operator', body: 'Each PC gets a Wake-on-LAN packet.' }
];
const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? '' : 's'}`;

/** Action entries for the text typed after `>` (or in the plain box): verbs with a PC or lab target, make head, and fixed shortcuts. */
export function actionsFor(text: string, c: Ctx): Entry[] {
	const t = text.trim().toLowerCase();
	const out: Entry[] = [];
	for (const v of VERBS) {
		if (!v.verb.test(t)) continue;
		const target = t.replace(v.verb, '').trim();
		const tl = target.replace(/^lab\s+/, '');
		const cand = /^lab\b/.test(target) || !c.nodes.some((n) => n.host.toLowerCase().includes(target)) ? c.labs : [];
		const exact = cand.filter((l) => target && [l.name.toLowerCase(), l.name.toLowerCase().replace(/^lab\s+/, '')].includes(tl));
		const labs = exact.length ? exact : cand.filter((l) => target && fuzzy(l.name, tl) >= 400);
		const pcs = c.nodes.filter((n) => target && !/^lab\b/.test(target) && (n.host.toLowerCase() === target || n.node_id.toLowerCase() === target)).slice(0, 5);
		const near = pcs.length ? [] : c.nodes.filter((n) => target && !/^lab\b/.test(target) && n.host.toLowerCase().includes(target)).slice(0, 4);
		for (const n of [...pcs, ...near]) out.push(mk(v, `${v.word} ${n.host}`, [n.node_id], n.host));
		for (const l of labs.slice(0, 3)) {
			const ids = c.nodes.filter((n) => n.lab_id === l.id).map((n) => n.node_id);
			if (ids.length) out.push(mk(v, `${v.word} lab ${l.name}`, ids, `lab ${l.name} (${plural(ids.length, 'PC')})`));
		}
		if (!target) out.push({ id: `act:${v.word}`, kind: 'action', label: `${v.word} …`, sub: 'type a PC or "lab A"', need: v.need });
	}
	const mh = /^make\s+(.+?)(\s+head)?$/.exec(t);
	if (mh || /^make\b/.test(t) || /^head\b|switchover/.test(t)) {
		const target = mh?.[1] ?? '';
		for (const n of c.nodes.filter((x) => target && x.host.toLowerCase().includes(target)).slice(0, 4))
			out.push({ id: `act:head:${n.node_id}`, kind: 'action', label: `Make ${n.host} head`, sub: 'planned switchover', need: 'admin', act: { run: { type: 'head', id: n.node_id }, title: `Make ${n.host} the head?`, body: 'The cluster hands over to this standby. It must be a connected standby; jobs keep running.', confirm: 'Make head' } });
		if (!target) out.push({ id: 'act:head', kind: 'action', label: 'Make … head', sub: 'type a standby PC', need: 'admin' });
	}
	const st = /^set\s+(\S*)/.exec(t);
	if (st) out.push({ id: `act:set:${st[1]}`, kind: 'action', label: st[1] ? `Set ${st[1]}` : 'Set a tunable', sub: 'opens Settings', href: `/settings?t.q=${encodeURIComponent(st[1])}`, need: 'operator' });
	for (const f of [
		{ id: 'act:newjob', label: 'New job', sub: 'submit work', href: '/jobs/new', k: 'new job submit run', need: 'researcher' as Role },
		{ id: 'act:join', label: 'New join code', sub: 'add a PC', href: '/nodes', k: 'join code add pc enrol', need: 'operator' as Role },
		{ id: 'act:power', label: 'Power settings', sub: 'sleep and wake rules', href: '/settings', k: 'power sleep wake energy', need: 'operator' as Role }
	]) if (!t || fuzzy(`${f.label} ${f.k}`, t) > 0) out.push({ id: f.id, kind: 'action', label: f.label, sub: f.sub, href: f.href, need: f.need });
	return out;
}
function mk(v: (typeof VERBS)[number], label: string, ids: string[], what: string): Entry {
	const n = ids.length;
	return {
		id: `act:${v.word}:${ids.join(',')}`, kind: 'action', label, sub: n > 1 ? plural(n, 'PC') : undefined, need: v.need,
		act: { run: v.state ? { type: 'state', state: v.state, ids } : { type: 'power', action: v.power!, ids }, title: `${v.word} ${what}?`, body: v.body, confirm: v.word.replace(/ on$/, '') }
	};
}

export const PAGES: { href: string; label: string; keys: string; need?: Role }[] = [
	{ href: '/', label: 'Overview', keys: 'home dashboard' },
	{ href: '/nodes', label: 'Nodes', keys: 'pcs machines fleet' },
	{ href: '/map', label: 'Floor map', keys: 'labs seats layout' },
	{ href: '/live', label: 'Live', keys: 'canvas system activity' },
	{ href: '/jobs', label: 'Jobs', keys: 'work tasks queue' },
	{ href: '/jobs/new', label: 'New job', keys: 'submit run' },
	{ href: '/policies', label: 'Policies', keys: 'caps reserves timetable pools' },
	{ href: '/events', label: 'Events', keys: 'audit history' },
	{ href: '/events?view=logs', label: 'Logs', keys: 'log lines' },
	{ href: '/experiments', label: 'Experiments', keys: 'research runs compare' },
	{ href: '/alerts', label: 'Alerts', keys: 'slow flaky warnings' },
	{ href: '/report', label: 'Report', keys: 'export summary' },
	{ href: '/wall', label: 'Wall mode', keys: 'display kiosk' },
	{ href: '/settings', label: 'System', keys: 'settings tunables head standby power', need: 'operator' },
	{ href: '/users', label: 'Users', keys: 'accounts roles', need: 'admin' }
];
