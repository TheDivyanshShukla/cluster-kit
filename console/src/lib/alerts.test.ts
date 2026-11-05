import { expect, test } from 'bun:test';
import { alertOrder, mergeAlerts } from './alerts';
import type { Alert } from './api/schema';

const a = (id: number, o: Partial<Alert> = {}): Alert => ({ id, kind: 'slow_pc', severity: 'warn', node_id: null, detail: '', since_ms: id, last_ms: id, active: true, acked_by: null, ...o });

test('active before closed, worse before milder, then newest', () => {
	const rows = [a(1, { active: false, severity: 'bad' }), a(2, { severity: 'info' }), a(3), a(4, { severity: 'bad' })];
	expect(rows.sort(alertOrder).map((x) => x.id)).toEqual([4, 3, 2, 1]);
});
test('merge replaces by id', () => {
	const m = mergeAlerts([a(1), a(2)], [a(2, { active: false }), a(3)]);
	expect(m.map((x) => [x.id, x.active])).toEqual([[1, true], [2, false], [3, true]]);
});
