import { expect, test } from 'bun:test';
import { HaStatus, NodeRow } from './schema';

const node = { node_id: 'n1', host: 'pc-01', addr: '10.0.0.1', inventory: null, first_seen_ms: 1, last_seen_ms: 2 };
test('node power reads as a plain state or as {state, since_ms}', () => {
	expect(NodeRow.parse({ ...node, power: 'asleep' })).toMatchObject({ power: 'asleep' });
	expect(NodeRow.parse({ ...node, power: { state: 'waking', since_ms: 99 } })).toMatchObject({ power: 'waking', power_since_ms: 99 });
});
test('ha.head is optional', () => {
	const base = { role: 'primary', epoch: 1, last_seq: 1, durable_seq: 1, standbys: [], membership: 1, connected: 1, dispatching: true };
	expect(HaStatus.parse(base).head).toBeNull();
	expect(HaStatus.parse({ ...base, head: { node_id: 'n1', addr: 'a', since_ms: 5 } }).head?.since_ms).toBe(5);
});
