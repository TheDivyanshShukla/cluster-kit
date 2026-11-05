<script lang="ts" generics="T">
	import { ArrowDownAZ, ArrowUpAZ, Search, X } from '@lucide/svelte';
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
		<div class="thin -mx-3 flex items-center gap-x-4 overflow-x-auto px-3 pb-0.5" role="group" aria-label="Filters">
			{#each opts as { f, o } (f.key)}
				<div class="flex shrink-0 items-center gap-1" role="group" aria-label={f.label}>
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
			{/each}
		</div>
	{/if}
</div>
