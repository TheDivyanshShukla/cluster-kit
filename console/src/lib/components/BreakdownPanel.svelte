<script lang="ts">
	import { TriangleAlert } from '@lucide/svelte';
	import type { Breakdown } from '#lib/api/schema.ts';
	import BreakdownBar from '#lib/components/BreakdownBar.svelte';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import { fmtNum } from '#lib/chart.ts';
	import { rate } from '#lib/format.ts';
	import { compareValues } from '#lib/listing.ts';

	// The job's time breakdown: stacked bar plus a sortable per-PC table. Slow PCs (stragglers) are flagged by a marker and a tint.
	let { b }: { b: Breakdown } = $props();
	let sort = $state('rate');
	let dir = $state<'asc' | 'desc'>('asc');
	const slow = $derived(new Set(b.stragglers));
	const rows = $derived.by(() => {
		const sign = dir === 'desc' ? -1 : 1;
		return [...b.per_pc].sort((x, y) => sign * compareValues(x[sort as 'rate'] as number, y[sort as 'rate'] as number) || compareValues(x.host, y.host));
	});
	const cols: Col[] = [
		{ key: 'host', label: 'PC', w: 'minmax(56px,1.2fr)', sort: true },
		{ key: 'chunks', label: 'Chunks', w: '80px', right: true, sort: true },
		{ key: 'numbers', label: 'Numbers', w: 'minmax(72px,1fr)', right: true, sort: true },
		{ key: 'rate', label: 'Rate', w: '74px', right: true, sort: true },
		{ key: 'busy_s', label: 'Busy', w: '64px', right: true, sort: true }
	];
	const onsort = (c: string) => ([sort, dir] = c === sort ? [sort, dir === 'asc' ? 'desc' : 'asc'] : [c, c === 'host' ? 'asc' : 'desc']);
</script>

<div class="flex min-h-0 flex-1 flex-col">
	<div class="border-line shrink-0 border-b p-3"><BreakdownBar {b} /></div>
	<p class="text-muted-foreground num shrink-0 px-3 pt-2 text-xs">
		Chunk time: median {b.chunk_s.p50.toFixed(1)} s · 90th percentile {b.chunk_s.p90.toFixed(1)} s · longest {b.chunk_s.max.toFixed(1)} s
		{#if slow.size}· <span class="text-warn inline-flex items-center gap-1"><TriangleAlert class="size-3" aria-hidden="true" />{slow.size} slow PC{slow.size === 1 ? '' : 's'}</span>{/if}
	</p>
	<DataTable label="Per-PC work" {rows} {cols} rowKey={(r) => r.node_id} {sort} {dir} {onsort} rowH={32} minW={340} rowClass={(r) => (slow.has(r.node_id) ? 'bg-warn/10' : '')}>
		{#snippet cell(r, c)}
			{#if c.key === 'host'}<span class="flex items-center gap-1.5 truncate font-medium">{#if slow.has(r.node_id)}<TriangleAlert class="text-warn size-3.5 shrink-0" aria-label="Slow PC" />{/if}{r.host || r.node_id}</span>
			{:else if c.key === 'chunks'}<span class="num">{r.chunks.toLocaleString('en-IN')}</span>
			{:else if c.key === 'numbers'}<span class="num" title={r.numbers.toLocaleString('en-IN')}>{fmtNum(r.numbers)}</span>
			{:else if c.key === 'rate'}<span class="num">{rate(r.rate)}</span>
			{:else}<span class="num">{r.busy_s.toFixed(1)} s</span>{/if}
		{/snippet}
	</DataTable>
</div>
