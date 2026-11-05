// What a PC looks like and which job it is serving, from the data the head already streams.
import type { JobRow, NodeRow, Worker } from '../api/schema';
import { parseSelector, type Attrs, type Matcher } from './select';

export type Cls = 'busy' | 'idle' | 'use' | 'asleep' | 'waking' | 'lost' | 'cord';
export const CLASSES: Cls[] = ['busy', 'idle', 'use', 'cord', 'asleep', 'waking', 'lost'];
export const CLS_WORDS: Record<Cls, string> = { busy: 'working', idle: 'idle', use: 'in use by a person', cord: 'cordoned or draining', asleep: 'asleep', waking: 'waking', lost: 'lost' };

type NodeLike = Pick<NodeRow, 'online' | 'power' | 'state' | 'presence'>;
export function classOf(n: NodeLike, w?: Pick<Worker, 'busy' | 'presence'> | null): Cls {
	if (n.power === 'asleep') return 'asleep';
	if (n.power === 'waking') return 'waking';
	if (!n.online && !w) return 'lost';
	if (n.state === 'cordoned' || n.state === 'draining' || n.state === 'maintenance') return 'cord';
	if (w?.presence === 'busy' || n.presence === 'busy') return 'use';
	return w?.busy ? 'busy' : 'idle';
}

/** Up to 8 running jobs get a hue slot (by id); the rest are grey (-1). */
export function jobSlots(jobs: Pick<JobRow, 'id'>[]): Map<number, number> {
	return new Map([...jobs].sort((a, b) => a.id - b.id).map((j, i) => [j.id, i < 8 ? i : -1]));
}

/** The job a PC is working for: the most urgent running job whose selector matches it (else none). */
export function makeJobFinder(jobs: Pick<JobRow, 'id' | 'selector' | 'priority'>[]) {
	const list = [...jobs]
		.sort((a, b) => a.priority - b.priority || a.id - b.id)
		.map((j) => ({ id: j.id, m: j.selector.trim() ? parseSelector(j.selector) : (null as Matcher | null) }))
		.filter((j) => typeof j.m !== 'string') as { id: number; m: Matcher | null }[];
	return (a: Attrs): number | null => list.find((j) => !j.m || j.m(a))?.id ?? null;
}

export function attrsOf(n: NodeRow, labName: string | undefined, cls: Cls, job: number | null, threads: number): Attrs {
	return {
		...n.labels, host: n.host, node: n.node_id, lab: labName, state: [n.state, cls], online: String(n.online), power: n.power,
		pool: n.pools, job: job == null ? undefined : String(job), threads: String(threads), cores: String(threads), class: n.class
	};
}
