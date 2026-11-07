<script lang="ts" module>
	import type { Breakdown } from '#lib/api/schema.ts';
	export type Cat = keyof Breakdown['pc_seconds'];
	/** One entry per pc_seconds category: label, colour, and what it means. Order = stacking order. */
	export const CATS: { key: Cat; label: string; color: string; help: string }[] = [
		{ key: 'compute', label: 'Compute', color: 'var(--primary)', help: 'PCs working on chunks that counted' },
		{ key: 'dispatch_wait', label: 'Waiting for work', color: '#E69F00', help: 'idle while the job still had unassigned work' },
		{ key: 'tail_idle', label: 'Tail', color: '#56B4E9', help: 'idle while only in-flight chunks remained' },
		{ key: 'recompute', label: 'Recomputed', color: 'var(--bad)', help: 'work discarded after a lost PC or a losing copy' },
		{ key: 'failover_pause', label: 'Failover pause', color: '#CC79A7', help: 'no head was dispatching' },
		{ key: 'other', label: 'Other', color: 'var(--idle)', help: 'the remainder' }
	];
</script>

<script lang="ts">
	// Stacked bar of where the PC-seconds went, with the numbers and percentages under it.
	let { b }: { b: Breakdown } = $props();
	const total = $derived(CATS.reduce((a, c) => a + b.pc_seconds[c.key], 0) || 1);
	const pcs = $derived(Math.max(1, Math.round(total / Math.max(b.wall_s, 1e-9))));
</script>

<div class="flex h-4 w-full overflow-hidden rounded-full" role="img" aria-label="Where the PC-seconds went">
	{#each CATS as c (c.key)}
		{#if b.pc_seconds[c.key] > 0}<div style="width:{(b.pc_seconds[c.key] / total) * 100}%;background:{c.color}" title="{c.label}: {Math.round((b.pc_seconds[c.key] / total) * 100)} %"></div>{/if}
	{/each}
</div>
<p class="text-muted-foreground num mt-1.5 text-xs">
	{Math.round(total).toLocaleString('en-IN')} PC-seconds = {pcs} PCs × {b.wall_s.toFixed(1)} s · efficiency <span class="text-foreground font-medium">{(b.efficiency * 100).toFixed(1)} %</span>
</p>
<dl class="mt-2 grid grid-cols-1 gap-x-4 gap-y-1 text-[13px] sm:grid-cols-2">
	{#each CATS as c (c.key)}
		<div class="flex items-baseline gap-2" title={c.help}>
			<span class="size-2.5 shrink-0 translate-y-px rounded-sm" style="background:{c.color}"></span>
			<dt class="truncate">{c.label}</dt>
			<dd class="num ml-auto whitespace-nowrap">
				{Math.round(b.pc_seconds[c.key]).toLocaleString('en-IN')} <span class="text-muted-foreground text-xs">PC-s</span>
				<span class="text-muted-foreground inline-block min-w-12 text-right">{((b.pc_seconds[c.key] / total) * 100).toFixed(1)} %</span>
			</dd>
		</div>
	{/each}
</dl>
