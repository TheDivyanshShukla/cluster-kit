// Day arithmetic for the report page. Days are local calendar days as YYYY-MM-DD, like the head's CK_TZ_OFFSET_MIN.
const pad = (n: number) => String(n).padStart(2, '0');
export const dayOf = (d: Date): string => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
export const validDay = (s: string | null): s is string => !!s && /^\d{4}-\d{2}-\d{2}$/.test(s) && dayOf(new Date(s + 'T12:00:00')) === s;
export function shiftDay(s: string, by: number): string {
	const d = new Date(s + 'T12:00:00');
	d.setDate(d.getDate() + by);
	return dayOf(d);
}
