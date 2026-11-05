// Pure display helpers. Missing values render as an em dash, never as 0.
const dash = '—';

export function rate(perSec: number | null | undefined): string {
	if (perSec == null || !Number.isFinite(perSec) || perSec <= 0) return dash;
	if (perSec >= 1e9) return `${(perSec / 1e9).toFixed(2)} G/s`;
	if (perSec >= 1e6) return `${(perSec / 1e6).toFixed(1)} M/s`;
	if (perSec >= 1e3) return `${(perSec / 1e3).toFixed(1)} k/s`;
	return `${Math.round(perSec)} /s`;
}

export function bytes(b: number | null | undefined): string {
	if (b == null || !Number.isFinite(b) || b <= 0) return dash;
	const gib = b / 2 ** 30;
	return gib >= 1 ? `${gib.toFixed(1)} GiB` : `${(b / 2 ** 20).toFixed(0)} MiB`;
}

export function pct(x: number | null | undefined): string {
	return x == null || !Number.isFinite(x) ? dash : `${Math.round(x)}%`;
}

export function count(n: number | null | undefined): string {
	return n == null || !Number.isFinite(n) ? dash : n.toLocaleString('en-IN');
}

export function duration(ms: number | null | undefined): string {
	if (ms == null || !Number.isFinite(ms) || ms < 0) return dash;
	const s = Math.round(ms / 1000);
	if (s < 60) return `${s} s`;
	if (s < 3600) return `${Math.floor(s / 60)} m ${s % 60} s`;
	return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} m`;
}

export function ago(ms: number | null | undefined, now = Date.now()): string {
	if (ms == null) return dash;
	const d = duration(Math.max(0, now - ms));
	return d === dash ? dash : d === '0 s' ? 'just now' : `${d} ago`;
}

export function datetime(ms: number | null | undefined): string {
	return ms == null ? dash : new Date(ms).toLocaleString('en-GB', { dateStyle: 'medium', timeStyle: 'medium' });
}
