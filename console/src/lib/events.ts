// One place for what an event kind is called in words (rail, Events page). Unknown kinds fall back to a tidied version
// of the kind itself, so a new kind from the head still reads sensibly. Tested in events.test.ts.
export const EVENT_LABELS: Record<string, string> = {
	'node.lost': 'PC lost', 'node.join': 'PC joined', 'node.joined': 'PC joined', 'node.connect': 'PC connected', 'node.disconnect': 'PC disconnected',
	'node.admitted': 'PC approved', 'node.approve': 'PC approved', 'node.denied': 'PC denied', 'node.state': 'PC state changed', 'node.placed': 'PC placed on the map',
	'node.error': 'PC error', 'node.key': 'PC key changed', 'node.key.tmp': 'PC key changed', 'node.labels': 'Labels changed',
	'job.submitted': 'Job submitted', 'job.submit': 'Job submitted', 'job.started': 'Job started', 'job.start': 'Job started', 'job.resume': 'Job resumed',
	'job.done': 'Job finished', 'job.complete': 'Job finished', 'job.failed': 'Job failed', 'job.error': 'Job error', 'job.cancelled': 'Job cancelled', 'job.cancel': 'Job cancelled',
	'chunk.start': 'Chunk started', 'chunk.end': 'Chunk finished', 'chunk.done': 'Chunk finished', 'chunk.dispatch': 'Chunk sent', 'chunk.failed': 'Chunk failed',
	'chunk.recovered': 'Chunk recovered', 'chunk.requeue': 'Chunk requeued', 'chunk.speculate': 'Chunk copied to a faster PC', 'chunk.speculate_won': 'Copy finished first', 'chunk.overlap': 'Chunk overlap',
	'task.timeout': 'Task timed out',
	'power.sleep': 'PCs went to sleep', 'power.wake': 'PC woke up', 'power.wol': 'Wake signal sent', 'power.eco': 'Eco mode', 'power.settings': 'Power settings changed',
	'power.error': 'Power error', 'power.sleep_failed': 'Sleep failed',
	'ha.promote': 'New head', 'ha.promoted': 'New head', 'ha.recruit': 'Standby recruited', 'ha.switchover': 'Head handed over', 'ha.standby_join': 'Standby joined',
	'ha.standby_leave': 'Standby left', 'ha.quorum': 'Quorum changed', 'ha.lag': 'Standby lagging', 'ha.fence': 'Old head fenced', 'ha.candidate': 'Head election', 'ha.settings': 'Head settings changed',
	'user.login': 'Signed in', 'user.login_failed': 'Sign-in failed', 'user.created': 'User created', 'user.create': 'User created', 'user.bootstrap': 'First admin created', 'auth.login': 'Signed in',
	'config.applied': 'Config applied', 'tune.changed': 'Tunable changed', 'tune.sent': 'Tunables sent to PCs', 'time.rule': 'Timetable rule',
	'reserve.added': 'Reserve added', 'reserve.changed': 'Reserve changed', 'reserve.deleted': 'Reserve removed',
	'window.added': 'Timetable rule added', 'window.deleted': 'Timetable rule removed', 'cap.added': 'Cap added', 'cap.deleted': 'Cap removed',
	'pool.put': 'Pool saved', 'pool.change': 'Pool saved', 'pool.deleted': 'Pool removed', 'label.set': 'Labels changed',
	'blob.put': 'File stored', 'blob.fetch': 'File fetched', 'blob.fetch_failed': 'File fetch failed', 'blob.port': 'File port opened',
	'experiment.uploaded': 'Experiment uploaded', 'alert.open': 'Alert opened', 'alert.close': 'Alert closed', 'api.request': 'API request'
};
export const eventLabel = (kind: string): string => {
	const known = EVENT_LABELS[kind];
	if (known) return known;
	const s = kind.replace(/[._]+/g, ' ').trim();
	return s ? s[0]!.toUpperCase() + s.slice(1) : 'Event';
};
