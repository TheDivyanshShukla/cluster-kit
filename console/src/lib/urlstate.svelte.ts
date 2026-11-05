// Small URL-query helpers: the active tab, list filters and sort live in the URL (shareable, survives reload).
// SvelteKit's page.url does not follow replaceState, so `cur` mirrors the query string reactively.
import { replaceState } from '$app/navigation';
import { page } from '$app/state';

export const cur = $state({ search: typeof location === 'undefined' ? '' : location.search });
/** the live query string as params */
export const params = () => new URLSearchParams(cur.search);
/** call from a component: re-reads the address bar after a real navigation */
export function followRouter() {
	$effect.pre(() => {
		void page.url.search;
		cur.search = location.search;
	});
}

/** Change the query string in place (no history entry, no reload). */
export function patchQuery(fn: (p: URLSearchParams) => URLSearchParams | void) {
	try {
		const u = new URL(location.href);
		const next = fn(u.searchParams) ?? u.searchParams;
		const qs = next.toString();
		replaceState(u.pathname + (qs ? `?${qs}` : '') + u.hash, page.state);
		cur.search = qs ? `?${qs}` : '';
	} catch {
		// the router is not ready yet: the next change will write it
	}
}
export const setQuery = (key: string, value: string | null) =>
	patchQuery((p) => {
		if (value == null || value === '') p.delete(key);
		else p.set(key, value);
	});
