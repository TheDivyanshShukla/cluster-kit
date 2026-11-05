<script lang="ts" generics="T">
	import { ArrowDownAZ, ArrowUpAZ, ListFilter, Search, X } from '@lucide/svelte';
	import type { Listing } from '#lib/listing.svelte.ts';

	// The one toolbar every list uses: search (150 ms debounce, `/` to focus, Esc to clear), facet chips with counts,
	// "Showing x of y" and Clear. Sort lives in the table header; `sorts` adds a picker for lists without a header.
	interface Props {
		l: Listing<T>;
		/** every row, before filtering: chip counts come from it */
		rows: T[];
		shown: number;
		total: number;
		placeholder?: string;
		/** only these facets (default: all) */
		show?: string[];
		sorts?: { key: string; label: string }[];
		hotkey?: boolean;
	}
	let { l, rows, shown, total, placeholder = 'Search', show, sorts, hotkey = true }: Props = $props();
	let input = $state<HTMLInputElement>();
	const facets = $derived(l.spec.facets.filter((f) => !show || show.includes(f.key)));
	const opts = $derived(facets.map((f) => ({ f, o: l.options(rows, f.key) })).filter((x) => x.o.length > 0));

	// Chips wrap to at most two rows on desktop (one scrollable row with edge fades on a phone); when facets do not fit,
	// a Filters popover lists every one of them.
	let strip = $state<HTMLDivElement>();
	let clipped = $state(false);
	let fade = $state({ l: false, r: false });
	let pop = $state(false);
	let at = $state({ top: 0, left: 0 });
	let btn = $state<HTMLButtonElement>();
	const measure = () => {
		const s = strip;
		if (!s) return;
		clipped = s.scrollHeight > s.clientHeight + 2 || s.scrollWidth > s.clientWidth + 2;
		fade = { l: s.scrollLeft > 2, r: s.scrollLeft + s.clientWidth < s.scrollWidth - 2 };
	};
	$effect(() => {
		void opts;
		const s = strip;
		if (!s) return;
		const ro = new ResizeObserver(measure);
		ro.observe(s);
		queueMicrotask(measure);
		return () => ro.disconnect();
	});
	function openPop() {
		const r = btn!.getBoundingClientRect();
		const w = Math.min(448, innerWidth - 16);
		at = { top: r.bottom + 4, left: Math.max(8, Math.min(r.right - w, innerWidth - w - 8)) };
		pop = !pop;
	}
	const nActive = $derived(Object.values(l.state.f).reduce((a, v) => a + v.length, 0));
	const mask = $derived(`${fade.l ? 'linear-gradient(to right, transparent, #000 20px' : 'linear-gradient(to right, #000 0'}, ${fade.r ? '#000 calc(100% - 20px), transparent)' : '#000 100%)'}`);

	function key(e: KeyboardEvent) {
		const t = e.target as HTMLElement | null;
		if (!hotkey || e.key !== '/' || !input || input.offsetParent === null || t?.closest('input,textarea,select,[contenteditable]')) return;
		e.preventDefault();
		input.focus();
	}
</script>

<svelte:window onkeydown={key} />

<div class="flex min-w-0 flex-col gap-2">
	<div class="flex min-w-0 items-center gap-2">
		<label class="bg-surface border-line focus-within:ring-ring flex min-w-0 flex-1 items-center gap-2 rounded-lg border px-2.5 focus-within:ring-2 max-sm:min-h-9 sm:max-w-sm">
			<Search class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />
			<span class="sr-only">{placeholder}</span>
			<input
				bind:this={input}
				type="search"
				value={l.input}
				oninput={(e) => l.setInput(e.currentTarget.value)}
				onkeydown={(e) => e.key === 'Escape' && l.input && (e.stopPropagation(), l.setInput(''))}
				{placeholder}
				autocomplete="off"
				spellcheck="false"
				class="h-8 w-full min-w-0 bg-transparent text-[16px] outline-none md:text-[13px] [&::-webkit-search-cancel-button]:hidden"
			/>
			{#if l.input}<button type="button" class="text-muted-foreground hover:text-foreground -mr-1 grid size-6 shrink-0 place-items-center" aria-label="Clear search" onclick={() => l.setInput('')}><X class="size-3.5" aria-hidden="true" /></button>{/if}
		</label>
		{#if sorts}
			<label class="flex items-center gap-1 text-xs">
				<span class="sr-only">Sort by</span>
				<select
					class="bg-surface border-line h-8 rounded-lg border px-2 text-[13px]"
					value={l.state.sort}
					onchange={(e) => l.setSort(e.currentTarget.value, l.state.dir)}
				>
					<option value="">Default order</option>
					{#each sorts as s (s.key)}<option value={s.key}>{s.label}</option>{/each}
				</select>
				{#if l.state.sort}
					<button type="button" class="border-line hover:bg-raised grid size-8 place-items-center rounded-lg border" aria-label={l.state.dir === 'asc' ? 'Ascending' : 'Descending'} onclick={() => l.setSort(l.state.sort, l.state.dir === 'asc' ? 'desc' : 'asc')}>
						{#if l.state.dir === 'asc'}<ArrowDownAZ class="size-4" aria-hidden="true" />{:else}<ArrowUpAZ class="size-4" aria-hidden="true" />{/if}
					</button>
				{/if}
			</label>
		{/if}
		<p class="text-muted-foreground num ml-auto shrink-0 text-xs" role="status">
			Showing <span class="text-foreground font-medium">{shown.toLocaleString('en-IN')}</span> of {total.toLocaleString('en-IN')}
		</p>
		{#if l.filtered}
			<button type="button" class="text-primary shrink-0 text-xs font-medium hover:underline max-sm:min-h-9" onclick={() => l.clear()}>Clear</button>
		{/if}
	</div>
	{#if opts.length}
		<div class="flex min-w-0 items-start gap-2">
			<div
				bind:this={strip}
				onscroll={measure}
				class="thin flex min-w-0 flex-1 items-center gap-x-4 gap-y-1 overflow-x-auto max-sm:flex-nowrap sm:max-h-[3.6rem] sm:flex-wrap sm:overflow-x-hidden sm:overflow-y-hidden"
				style="mask-image:{mask};-webkit-mask-image:{mask}"
				role="group"
				aria-label="Filters"
			>
				{#each opts as { f, o } (f.key)}
					{@render facet(f, o)}
				{/each}
			</div>
			{#if clipped || nActive}
				<button bind:this={btn} type="button" class="border-line hover:bg-raised inline-flex h-6 shrink-0 items-center gap-1 rounded-full border px-2 text-xs max-sm:h-8 {pop ? 'bg-raised' : ''}" aria-expanded={pop} aria-haspopup="dialog" onclick={openPop}>
					<ListFilter class="size-3.5" aria-hidden="true" />Filters{#if nActive}<span class="num bg-primary text-primary-foreground rounded-full px-1.5 text-[11px]">{nActive}</span>{/if}
				</button>
			{/if}
		</div>
		{#if pop}
			<button type="button" class="fixed inset-0 z-40 cursor-default" aria-label="Close filters" tabindex="-1" onclick={() => (pop = false)}></button>
			<div class="bg-surface border-line thin fixed z-50 max-h-[60dvh] overflow-auto rounded-lg border p-3 shadow-lg" style="top:{at.top}px;left:{at.left}px;width:min(28rem,calc(100vw - 1rem))" role="dialog" aria-label="All filters">
				<div class="flex flex-col gap-3">
					{#each opts as { f, o } (f.key)}
						<div class="flex flex-col gap-1">{@render facet(f, o, true)}</div>
					{/each}
				</div>
			</div>
		{/if}
	{/if}
</div>

{#snippet facet(f: (typeof facets)[number], o: ReturnType<typeof l.options>, wrap = false)}
	<div class="flex items-center gap-1 {wrap ? 'flex-wrap' : 'shrink-0'}" role="group" aria-label={f.label}>
		<span class="text-muted-foreground mr-0.5 text-[11px] tracking-wide uppercase">{f.label}</span>
		{#each o as c (c.value)}
			<button
				type="button"
				aria-pressed={c.active}
				onclick={() => l.toggle(f.key, c.value)}
				class="inline-flex h-6 items-center gap-1 rounded-full border px-2 text-xs whitespace-nowrap max-sm:h-8 {c.active
					? 'border-primary bg-primary/10 text-foreground font-medium'
					: 'border-line text-muted-foreground hover:bg-raised hover:text-foreground'} {c.count === 0 && !c.active ? 'opacity-50' : ''}"
			>
				{f.text?.(c.value) ?? c.value}<span class="num opacity-70">{c.count}</span>
			</button>
		{/each}
	</div>
{/snippet}
