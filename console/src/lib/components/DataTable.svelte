<script lang="ts" module>
	export interface Col {
		key: string;
		label: string;
		/** CSS grid track, e.g. '1fr' or '90px' */
		w: string;
		right?: boolean;
		/** sortable (the key is the sorter name) */
		sort?: boolean;
	}
</script>

<script lang="ts" generics="T">
	import { ArrowDown, ArrowUp, ArrowUpDown } from '@lucide/svelte';
	import type { Snippet } from 'svelte';
	import ScrollFade from '#lib/components/ScrollFade.svelte';
	import { windowRange, WINDOW_OVER } from '#lib/listing.ts';

	// A table that fills its panel: sticky sortable header, fixed row height, windowed above 500 rows, keeps your
	// scroll position when rows arrive on top and shows an "N new" pill instead of jumping.
	interface Props {
		rows: T[];
		cols: Col[];
		rowKey: (r: T) => string | number;
		cell: Snippet<[T, Col, number]>;
		sort?: string;
		dir?: 'asc' | 'desc';
		onsort?: (col: string) => void;
		rowH?: number;
		/** px; the table scrolls sideways inside its panel below this */
		minW?: number;
		active?: (r: T) => boolean;
		onrow?: (r: T) => void;
		/** called when the last rows come into view (load older) */
		onend?: () => void;
		/** show the "N new" pill when rows arrive above while scrolled down */
		pill?: boolean;
		empty?: Snippet;
		label: string;
		rowClass?: (r: T) => string;
	}
	let { rows, cols, rowKey, cell, sort = '', dir = 'asc', onsort, rowH = 40, minW = 0, active, onrow, onend, pill = false, empty, label, rowClass }: Props = $props();

	let el = $state<HTMLElement>();
	let viewH = $state(600);
	let top = $state(0);
	let fresh = $state(0);
	const tpl = $derived(cols.map((c) => c.w).join(' '));
	const HEAD = 32;
	const windowed = $derived(rows.length > WINDOW_OVER);
	const win = $derived(windowed ? windowRange(top, viewH - HEAD, rowH, rows.length) : { start: 0, end: rows.length });
	const slice = $derived(rows.slice(win.start, win.end));

	// rows added above while scrolled down: hold the view still, count them
	let firstKey: string | number | undefined;
	$effect(() => {
		const k = rows.length ? rowKey(rows[0]!) : undefined;
		if (firstKey !== undefined && k !== firstKey && el) {
			let added = 0;
			for (let i = 0; i < Math.min(rows.length, 500); i++) if (rowKey(rows[i]!) === firstKey) ((added = i), (i = 1e9));
			if (added > 0 && el.scrollTop > rowH / 2) {
				el.scrollTop += added * rowH;
				if (pill) fresh += added;
			}
		}
		firstKey = k;
	});
	function onscroll() {
		if (el && el.scrollTop < rowH / 2) fresh = 0;
		if (onend && el && el.scrollTop + el.clientHeight > el.scrollHeight - rowH * 4) onend();
	}
	$effect(() => {
		// a short list that does not fill the panel still asks for more
		void rows.length;
		if (onend && el && el.scrollHeight <= el.clientHeight + 4) onend();
	});
	const toTop = () => {
		if (el) el.scrollTop = 0;
		fresh = 0;
	};
</script>

<div class="relative flex min-h-0 flex-1 flex-col">
	<ScrollFade bind:el bind:viewH bind:top {onscroll} fadeTop={HEAD}>
		<div role="table" aria-label={label} aria-rowcount={rows.length + 1} style="min-width:{minW}px">
			<div role="row" class="bg-surface border-line text-muted-foreground sticky top-0 z-10 grid items-center border-b text-xs" style="grid-template-columns:{tpl};height:{HEAD}px">
				{#each cols as c (c.key)}
					<div role="columnheader" aria-sort={c.sort && sort === c.key ? (dir === 'asc' ? 'ascending' : 'descending') : undefined} class="min-w-0 px-3 max-sm:px-2 {c.right ? 'text-right' : ''}">
						{#if c.sort && onsort}
							<button type="button" class="hover:text-foreground inline-flex max-w-full items-center gap-1 font-medium max-sm:min-h-9 {sort === c.key ? 'text-foreground' : ''}" onclick={() => onsort(c.key)}>
								<span class="truncate">{c.label}</span>
								{#if sort === c.key}{#if dir === 'asc'}<ArrowUp class="size-3 shrink-0" aria-hidden="true" />{:else}<ArrowDown class="size-3 shrink-0" aria-hidden="true" />{/if}{:else}<ArrowUpDown class="size-3 shrink-0 opacity-40" aria-hidden="true" />{/if}
							</button>
						{:else}<span class="truncate font-medium">{c.label}</span>{/if}
					</div>
				{/each}
			</div>
			{#if rows.length === 0}
				{#if empty}{@render empty()}{/if}
			{:else}
				<div style={windowed ? `height:${rows.length * rowH}px;position:relative` : ''}>
					<div style={windowed ? `position:absolute;inset-inline:0;top:${win.start * rowH}px` : ''}>
						{#each slice as r, i (rowKey(r))}
							{@const idx = win.start + i}
							<div
								role="row"
								aria-rowindex={idx + 2}
								class="border-line grid items-center border-b text-[13px] {onrow ? 'hover:bg-raised cursor-pointer' : ''} {active?.(r) ? 'bg-primary/10' : ''} {rowClass?.(r) ?? ''}"
								style="grid-template-columns:{tpl};height:{rowH}px"
								onclick={onrow ? (e) => !(e.target as HTMLElement).closest('a,button,input,label') && onrow(r) : undefined}
								onkeydown={onrow ? (e) => (e.key === 'Enter' || e.key === ' ') && e.target === e.currentTarget && (e.preventDefault(), onrow(r)) : undefined}
								tabindex={onrow ? 0 : undefined}
							>
								{#each cols as c (c.key)}
									<div role="cell" class="min-w-0 px-3 max-sm:px-2 {c.right ? 'text-right' : ''}">{@render cell(r, c, idx)}</div>
								{/each}
							</div>
						{/each}
					</div>
				</div>
			{/if}
		</div>
	</ScrollFade>
	{#if fresh > 0}
		<button type="button" class="bg-primary text-primary-foreground absolute top-10 left-1/2 z-30 -translate-x-1/2 rounded-full px-3 py-1 text-xs font-medium shadow-lg" onclick={toTop}>{fresh} new</button>
	{/if}
</div>
