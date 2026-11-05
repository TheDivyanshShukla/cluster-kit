import { expect, test } from 'bun:test';
import { eventLabel, EVENT_LABELS } from './events';
import { railText } from './canvas/effects';

test('every kind the head writes has a human label', () => {
	// kinds from rust/src (rg for event kinds); keep in step when the head adds one
	const kinds = ['node.lost', 'node.joined', 'node.state', 'node.admitted', 'node.denied', 'node.placed', 'job.submitted', 'job.started', 'job.done', 'job.failed', 'job.cancelled', 'job.resume',
		'chunk.recovered', 'task.timeout', 'power.sleep', 'power.wake', 'power.wol', 'power.eco', 'power.settings', 'power.error', 'ha.promoted', 'ha.recruit', 'ha.switchover', 'ha.standby_join', 'ha.standby_leave',
		'ha.quorum', 'ha.lag', 'ha.fence', 'ha.settings', 'user.login', 'user.created', 'user.bootstrap', 'config.applied', 'tune.changed', 'time.rule', 'reserve.added', 'reserve.deleted', 'window.added',
		'cap.added', 'pool.put', 'label.set', 'blob.put', 'experiment.uploaded', 'alert.open', 'alert.close'];
	for (const k of kinds) expect(EVENT_LABELS[k], k).toBeTruthy();
});
test('unknown kinds are tidied, and the rail prefers the head text', () => {
	expect(eventLabel('widget.rotated_fast')).toBe('Widget rotated fast');
	expect(eventLabel('')).toBe('Event');
	expect(railText({ kind: 'node.lost', detail: 'pc-30 lost' })).toBe('pc-30 lost');
	expect(railText({ kind: 'node.lost', detail: '' })).toBe('PC lost');
});
