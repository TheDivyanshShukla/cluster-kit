import type { Alert } from '#lib/api/schema.ts';

export const KIND_LABEL: Record<Alert['kind'], string> = {
	slow_pc: 'Slow PC',
	flaky_pc: 'Flaky PC',
	head_hot: 'Head busy',
	standby_behind: 'Standby behind',
	reserve_at_risk: 'Reserve at risk',
	no_standby: 'No standby'
};
export const SEVERITIES: Alert['severity'][] = ['bad', 'warn', 'info'];
export const SEV_LABEL: Record<Alert['severity'], string> = { bad: 'Critical', warn: 'Warning', info: 'Notice' };

/** Active first, then worst severity, then newest. */
export const alertOrder = (a: Alert, b: Alert) =>
	Number(b.active) - Number(a.active) || SEVERITIES.indexOf(a.severity) - SEVERITIES.indexOf(b.severity) || b.since_ms - a.since_ms;

/** An alert update from the stream or a poll replaces the row with the same id. */
export const mergeAlerts = (old: Alert[], fresh: Alert[]): Alert[] => {
	const m = new Map(old.map((a) => [a.id, a]));
	for (const a of fresh) m.set(a.id, a);
	return [...m.values()];
};
