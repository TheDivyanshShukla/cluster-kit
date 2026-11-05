import { expect, test } from 'bun:test';
import { hostIndex } from './effects';
import { headAt, rewind, type NodeSnap } from './replay';

const hosts = hostIndex([{ node_id: 'n1', host: 'pc-01' }, { node_id: 'n2', host: 'pc-02' }, { node_id: 'n3', host: 'pc-03' }]);
const up: NodeSnap = { online: true, power: 'awake', state: 'active' };
const ev = (id: number, ts_ms: number, kind: string, detail: string) => ({ id, ts_ms, kind, detail, actor: '' });

test('rewinding undoes later events and leaves earlier ones', () => {
	const now = { n1: { ...up, online: false }, n2: { ...up, online: false, power: 'asleep' as const }, n3: { ...up } };
	const events = [ev(1, 100, 'node.lost', 'pc-03 lost'), ev(2, 200, 'node.lost', 'pc-01 lost'), ev(3, 300, 'power.sleep', 'pc-02 went to sleep')];
	const at250 = rewind(now, events, 250, hosts);
	expect(at250.n2).toEqual({ ...up });
	expect(at250.n1).toBeUndefined(); // lost at 200, before 250: still lost
	const at50 = rewind(now, events, 50, hosts);
	expect(at50.n1.online).toBe(true);
	expect(at50.n3.online).toBe(true);
	expect(now.n2.power).toBe('asleep'); // input untouched
});
test('multiple changes to one PC are undone newest first', () => {
	const now = { n1: { ...up } };
	const events = [ev(1, 100, 'node.lost', 'pc-01 lost'), ev(2, 200, 'node.join', 'pc-01 joined')];
	expect(rewind(now, events, 150, hosts).n1.online).toBe(false);
	expect(rewind(now, events, 50, hosts).n1.online).toBe(true);
});
test('headAt follows promotions', () => {
	const events = [ev(1, 100, 'ha.promote', 'pc-02 is the head, term 4'), ev(2, 300, 'ha.promote', 'pc-03 is the head, term 5')];
	expect(headAt(events, 50, hosts)).toBeNull();
	expect(headAt(events, 200, hosts)).toBe('n2');
	expect(headAt(events, 400, hosts)).toBe('n3');
});
