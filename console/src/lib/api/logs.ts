// GET /api/v1/logs (operator): JSON log lines, docs/LOGGING.md. Fixed prefix plus the fields of that kind.
import { z } from 'zod';

export const LogLine = z.looseObject({
	ts_ms: z.number(),
	level: z.enum(['error', 'warn', 'info', 'debug']),
	role: z.string().default(''),
	node: z.string().default(''),
	kind: z.string()
});
export type LogLine = z.infer<typeof LogLine>;
export const LogList = z.object({ logs: z.array(LogLine) });
export const LEVELS = ['error', 'warn', 'info', 'debug'] as const;

/** One line of text for the table: msg, else detail, else the remaining fields. */
export function logSummary(l: LogLine): string {
	const { ts_ms: _t, level: _l, role: _r, node: _n, kind: _k, msg, detail, ...rest } = l as Record<string, unknown>;
	if (typeof msg === 'string' && msg) return msg;
	if (typeof detail === 'string' && detail) return detail;
	return Object.entries(rest).map(([k, v]) => `${k}=${typeof v === 'object' ? JSON.stringify(v) : v}`).join(' ');
}
