import { expect, test } from 'bun:test';
import { effectFor, hostIndex, railKind, schedule, MAX_EFFECTS, type Live } from './effects';

const hosts = hostIndex([{ node_id: 'n006', host: 'pc-06' }, { node_id: 'n030', host: 'pc-30' }, { node_id: 'n018', host: 'pc-18' }]);

test('event kinds map to their effect and find the PC by host', () => {
	expect(effectFor({ kind: 'node.lost', actor: '', detail: 'pc-30 lost' }, hosts)).toEqual({ type: 'ripple', tone: 'bad', ids: ['n030'] });
	expect(effectFor({ kind: 'node.join', actor: '', detail: 'pc-30 joined' }, hosts)).toEqual({ type: 'ripple', tone: 'ok', ids: ['n030'] });
	expect(effectFor({ kind: 'power.wake', actor: '', detail: 'pc-06 woke up, 14:03' }, hosts)).toEqual({ type: 'glyph', glyph: 'sunrise', ids: ['n006'] });
	expect(effectFor({ kind: 'power.sleep', actor: '', detail: '3 PCs went to sleep, 06:10' }, hosts)).toEqual({ type: 'glyph', glyph: 'moon', ids: [] });
	expect(effectFor({ kind: 'ha.promote', actor: '', detail: 'pc-18 is the head, term 4' }, hosts)).toEqual({ type: 'failover', ids: ['n018'] });
	expect(effectFor({ kind: 'ha.recruit', actor: '', detail: 'pc-18 became a standby' }, hosts)?.type).toBe('recruit');
	expect(effectFor({ kind: 'spec.copy', actor: '', detail: 'pc-06' }, hosts)?.type).toBe('spec');
	expect(effectFor({ kind: 'auth.login', actor: 'a', detail: 'login' }, hosts)).toBeNull();
});
test('rail kinds carry a tone', () => {
	expect(railKind('node.lost').tone).toBe('bad');
	expect(railKind('job.done').key).toBe('job');
	expect(railKind('zzz').key).toBe('other');
});
test('scheduling drops finished effects and caps the list', () => {
	let l: Live[] = [];
	for (let i = 0; i < 200; i++) l = schedule(l, { type: 'ripple', tone: 'ok', ids: ['n006'] }, 1000);
	expect(l.length).toBe(MAX_EFFECTS);
	expect(schedule(l, { type: 'ripple', tone: 'ok', ids: [] }, 99999).length).toBe(1);
	expect(schedule([], { type: 'ripple', tone: 'ok', ids: [] }, 0)[0].id).toBe('head');
});
