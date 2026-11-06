import { expect, test } from 'bun:test';
import { accuracyText, adviceOrder, changeLine, metricLine, modeOf, openCount, policyKinds, suggestionsText, verdict } from './advisor';
import { Suggestion, AdviceApplied } from './api/schema';

const s = (id: string, severity: 'warn' | 'tip' | 'info', since_ms: number) => Suggestion.parse({ id, kind: 'disk', severity, title: id, since_ms });

test('worst first, then longest standing', () => {
	const r = [s('a', 'tip', 1), s('b', 'warn', 9), s('c', 'warn', 2), s('d', 'info', 0)].sort(adviceOrder);
	expect(r.map((x) => x.id)).toEqual(['c', 'b', 'a', 'd']);
});
test('policy defaults to suggest; unknown kinds are listed', () => {
	expect(modeOf({ disk: 'off', eco: 'auto' }, 'disk')).toBe('off');
	expect(modeOf({}, 'eco')).toBe('suggest');
	const k = policyKinds({ zzz: 'off' }, ['qqq']).map((x) => x.kind);
	expect(k.slice(-2)).toEqual(['qqq', 'zzz']);
	expect(policyKinds({}, []).find((x) => x.kind === 'disk')?.auto).toBe(false);
});
test('text helpers', () => {
	expect(accuracyText(undefined)).toContain('not enough');
	expect(accuracyText(0.666)).toBe('67 % of past predictions held');
	expect(changeLine({ setting: 'target_chunk_s', from: 2, to: 4.126 })).toBe('target_chunk_s: 2 → 4.13');
	expect(metricLine('tail_share', 0.4, 0.25)).toBe('tail share 0.4 → 0.25');
	expect(suggestionsText(1)).toBe('1 suggestion');
	expect(suggestionsText(2)).toBe('2 suggestions');
});
test('verdicts and counts', () => {
	const a = (o: object) => AdviceApplied.parse({ id: 'x', kind: 'disk', option: 'o', ts_ms: 1, ...o });
	expect(verdict(a({}))).toBe('waiting');
	expect(verdict(a({ outcome: { hit: true } }))).toBe('held');
	expect(verdict(a({ outcome: { hit: false } }))).toBe('missed');
	expect(verdict(a({ undone: true, outcome: { hit: true } }))).toBe('undone');
	expect(openCount([{ ...s('a', 'tip', 1), policy: 'off' }, s('b', 'tip', 1)])).toBe(1);
	expect(openCount(null)).toBe(0);
});
import { actionLine } from './advisor';
test('action lines', () => {
	expect(actionLine({ type: 'tune', key: 'pipeline', from: 2, to: 3 })).toBe('pipeline 2 → 3');
	expect(actionLine({ type: 'node_state', node_ids: ['a', 'b'], from: 'active', to: 'draining' })).toBe('a, b active → draining');
	expect(actionLine({ type: 'doc', link: 'x' })).toBe('');
	expect(actionLine(null)).toBe('');
});
