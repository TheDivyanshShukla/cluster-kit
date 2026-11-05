// Reactive wrapper around lib/listing.ts: one list's search, chips and sort, kept in the URL.
import { emptyState, facetOptions, fromParams, isFiltered, nextSort, runListing, toggleFacet, toParams, type ListState, type Option, type Spec } from './listing';
import { patchQuery } from './urlstate.svelte';

export class Listing<T> {
	state = $state<ListState>(emptyState());
	/** what the search box shows; `state.q` follows it 150 ms later */
	input = $state('');
	#timer: ReturnType<typeof setTimeout> | undefined;

	constructor(
		readonly spec: Spec<T>,
		readonly prefix = ''
	) {
		this.state = fromParams(new URLSearchParams(location.search), spec, prefix);
		this.input = this.state.q;
	}
	#write() {
		patchQuery((p) => toParams(this.state, this.spec, p, this.prefix));
	}
	setInput(v: string) {
		this.input = v;
		clearTimeout(this.#timer);
		this.#timer = setTimeout(() => ((this.state.q = v), this.#write()), 150);
	}
	toggle(key: string, value: string) {
		this.state.f = toggleFacet(this.state, this.spec, key, value);
		this.#write();
	}
	sortBy(col: string) {
		Object.assign(this.state, nextSort(this.state, col));
		this.#write();
	}
	setSort(sort: string, dir: 'asc' | 'desc') {
		Object.assign(this.state, { sort, dir });
		this.#write();
	}
	clear() {
		clearTimeout(this.#timer);
		this.state.q = this.input = '';
		this.state.f = {};
		this.#write();
	}
	get filtered() {
		return isFiltered(this.state) || this.input.trim() !== '';
	}
	run(rows: T[]) {
		return runListing(rows, this.state, this.spec);
	}
	options(rows: T[], key: string): Option[] {
		return facetOptions(rows, this.state, this.spec, key);
	}
}
