<script lang="ts">
	import { live } from '#lib/live.svelte.ts';
	import { CircleAlert, GitCompareArrows, X } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Breakdown, JobRow } from '#lib/api/schema.ts';
	import { OKABE } from '#lib/chart.ts';
	import ChartLine from '#lib/components/ChartLine.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { duration, rate } from '#lib/format.ts';
	import { params, setQuery } from '#lib/urlstate.svelte.ts';
	import { field } from '#lib/ui.ts';

	const MAX = 4;
	const ids = $derived([...new Set((params().get('jobs') ?? '').split(',').map(Number).filter((n) => Number.isInteger(n) && n > 0))].slice(0, MAX));
	type Got = { job?: JobRow; bd?: Breakdown; err?: string };
	let got = $state<Record<number, Got>>({});
	let pool = $state<JobRow[]>([]);
	let poolErr = $state(false);

	$effect(() => {
		for (const id of ids) {
			if (got[id]) continue;
			got[id] = {};
			void Promise.allSettled([api.job(id), api.breakdown(id)]).then(([j, b]) => {
				got[id] = {
					job: j.status === 'fulfilled' ? j.value.job : undefined,
					bd: b.status === 'fulfilled' ? b.value.breakdown : undefined,
					err: j.status === 'rejected' ? (j.reason instanceof ApiError && j.reason.status === 404 ? `Job ${id} does not exist.` : 'Could not load this job.') : b.status === 'rejected' ? 'This job has no time breakdown yet (it has not started).' : undefined
				};
			});
		}
	});
	// running jobs refresh from the stream (at most every 2 s each); a job that just finished is read once more
	const seenAt: Record<number, number> = {};
	$effect(() => {
		const timers: ReturnType<typeof setTimeout>[] = [];
		for (const id of ids) {
			const j = live.jobs[id];
			const stale = j && got[id]?.job && (j.state === 'running' || got[id]!.job!.state !== j.state);
			if (!stale) continue;
			timers.push(setTimeout(() => {
				seenAt[id] = Date.now();
				void Promise.allSettled([api.job(id), api.breakdown(id)]).then(([a, b]) => {
					if (a.status === 'fulfilled') got[id] = { ...got[id], job: a.value.job, bd: b.status === 'fulfilled' ? b.value.breakdown : got[id]?.bd, err: undefined };
				});
			}, Math.max(0, 2000 - (Date.now() - (seenAt[id] ?? 0)))));
		}
		return () => timers.forEach(clearTimeout);
	});
	$effect(() => {
		api.jobs(100).then((r) => (pool = r.jobs.filter((j) => j.state !== 'queued'))).catch(() => (poolErr = true));
	});
	const set = (v: number[]) => setQuery('jobs', v.length ? v.join(',') : null);
	const add = (e: Event & { currentTarget: HTMLSelectElement }) => {
		const n = Number(e.currentTarget.value);
		e.currentTarget.value = '';
		if (n) set([...ids, n]);
	};
	const label = (id: number) => got[id]?.job?.name || `Job ${id}`;
	const cols = $derived(ids.map((id, i) => ({ id, i, color: OKABE[i]!, ...got[id] })));
	const ready = $derived(cols.filter((c) => c.bd));

	const total = (b: Breakdown) => Object.values(b.pc_seconds).reduce((a, x) => a + x, 0) || 1;
	const rows: { label: string; unit?: string; get: (b: Breakdown) => number; fmt: (v: number) => string; best?: 'min' | 'max' }[] = [
		{ label: 'Makespan', get: (b) => b.wall_s, fmt: (v) => duration(v * 1000), best: 'min' },
		{ label: 'Efficiency', get: (b) => b.efficiency * 100, fmt: (v) => `${v.toFixed(1)} %`, best: 'max' },
		{ label: 'PCs used', get: (b) => b.per_pc.length, fmt: (v) => String(v) },
		{ label: 'Mean rate', get: (b) => b.per_pc.reduce((a, p) => a + p.numbers, 0) / Math.max(b.wall_s, 1e-9), fmt: (v) => rate(v), best: 'max' },
		{ label: 'Straggler time', get: (b) => b.pc_seconds.tail_idle, fmt: (v) => `${Math.round(v).toLocaleString('en-IN')} PC-s`, best: 'min' },
		{ label: 'Share lost to the tail', get: (b) => (b.pc_seconds.tail_idle / total(b)) * 100, fmt: (v) => `${v.toFixed(1)} %`, best: 'min' },
		{ label: 'Recomputed', get: (b) => (b.pc_seconds.recompute / total(b)) * 100, fmt: (v) => `${v.toFixed(1)} %`, best: 'min' },
		{ label: 'Slow PCs', get: (b) => b.stragglers.length, fmt: (v) => String(v), best: 'min' }
	];
	const best = (r: (typeof rows)[number]) => {
		const v = ready.map((c) => r.get(c.bd!));
		return r.best && v.length > 1 ? (r.best === 'min' ? Math.min(...v) : Math.max(...v)) : null;
	};
	const stats = [['p50', 'Median'], ['p90', '90th percentile'], ['max', 'Longest']] as const;
	const maxChunk = $derived(Math.max(1e-9, ...ready.map((c) => c.bd!.chunk_s.max)));
	const rateSeries = $derived(
		ready.map((c) => ({
			name: `#${c.id} ${label(c.id)}`, color: c.color, dots: true,
			pts: [...c.bd!.per_pc].sort((a, b) => b.rate - a.rate).map((p, i) => ({ x: i + 1, y: p.rate / 1e6 }))
		}))
	);
	const free = $derived(pool.filter((j) => !ids.includes(j.id)));
</script>

<Page
	title="Compare jobs"
	subtitle="Makespan, efficiency, chunk times, per-PC rates and the time lost to stragglers, for 2 to 4 jobs."
	tabs={[{ id: 'summary', label: 'Summary' }, { id: 'chunks', label: 'Chunk times' }, { id: 'rates', label: 'PC rates' }]}
	class="sm:grid-cols-2 sm:grid-rows-[minmax(0,1fr)_minmax(0,1fr)]"
>
	<Panel title="Summary" tab="summary">
		{#snippet toolbar()}
			<div class="flex flex-wrap items-center gap-2">
				{#each cols as c (c.id)}
					<span class="border-line inline-flex h-7 items-center gap-1.5 rounded-full border pr-1 pl-2.5 text-xs">
						<span class="size-2.5 rounded-full" style="background:{c.color}"></span><span class="max-w-40 truncate"><span class="num">#{c.id}</span> {c.job?.name ?? ''}</span>
						<button type="button" class="hover:bg-raised grid size-5 place-items-center rounded-full max-sm:size-8" aria-label="Remove job {c.id}" onclick={() => set(ids.filter((x) => x !== c.id))}><X class="size-3" aria-hidden="true" /></button>
					</span>
				{/each}
				{#if ids.length < MAX}
					<label class="min-w-0">
						<span class="sr-only">Add a job</span>
						<select class="{field} h-9 w-48" onchange={add} disabled={poolErr || free.length === 0}>
							<option value="">{poolErr ? 'Jobs unavailable' : 'Add a job...'}</option>
							{#each free as j (j.id)}<option value={j.id}>#{j.id} {j.name || 'Untitled'}</option>{/each}
						</select>
					</label>
				{/if}
			</div>
		{/snippet}
		{#if ids.length === 0}
			<EmptyState icon={GitCompareArrows} title="Pick jobs to compare" body="Add two to four jobs above, or use Compare with... on a job's page." />
		{:else if cols.every((c) => !c.bd && !c.err)}
			<div class="bg-raised h-40 animate-pulse rounded-lg" aria-busy="true"></div>
		{:else}
			{#each cols.filter((c) => c.err) as c (c.id)}<p class="text-bad mb-2 flex items-start gap-1.5 text-[13px]" role="alert"><CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />#{c.id}: {c.err}</p>{/each}
			{#if ready.length}
				<table class="w-full border-collapse text-[13px]">
					<thead>
						<tr class="text-muted-foreground text-left text-xs">
							<th class="py-1.5 pr-3 font-medium"><span class="sr-only">Measure</span></th>
							{#each ready as c (c.id)}<th class="px-2 py-1.5 text-right font-medium" style="border-bottom:2px solid {c.color}"><span class="num">#{c.id}</span></th>{/each}
						</tr>
					</thead>
					<tbody>
						{#each rows as r (r.label)}
							{@const b = best(r)}
							<tr class="border-line border-b">
								<th class="py-1.5 pr-3 text-left font-normal">{r.label}</th>
								{#each ready as c (c.id)}
									{@const v = r.get(c.bd!)}
									<td class="num px-2 py-1.5 text-right {b !== null && v === b ? 'text-ok font-semibold' : ''}">{r.fmt(v)}</td>
								{/each}
							</tr>
						{/each}
					</tbody>
				</table>
				<p class="text-muted-foreground mt-2 text-xs">Green is the best of the compared jobs. Different job sizes and PC counts change the makespan: compare efficiency for fairness.</p>
			{/if}
		{/if}
	</Panel>

	<Panel title="Chunk times" tab="chunks">
		{#if ready.length === 0}
			<p class="text-muted-foreground px-2 py-6 text-center text-[13px]">Chunk times appear once jobs are chosen.</p>
		{:else}
			<div class="space-y-4">
				{#each stats as [k, name] (k)}
					<div>
						<p class="text-muted-foreground mb-1 text-xs">{name}</p>
						{#each ready as c (c.id)}
							<div class="flex items-center gap-2 py-0.5">
								<span class="num text-muted-foreground w-9 shrink-0 text-xs">#{c.id}</span>
								<div class="bg-raised h-3 min-w-0 flex-1 overflow-hidden rounded-sm"><div class="h-full" style="width:{(c.bd!.chunk_s[k] / maxChunk) * 100}%;background:{c.color}"></div></div>
								<span class="num w-14 shrink-0 text-right text-[13px]">{c.bd!.chunk_s[k].toFixed(2)} s</span>
							</div>
						{/each}
					</div>
				{/each}
			</div>
		{/if}
	</Panel>

	<Panel title="PC rates" tab="rates" scroll={false} class="sm:col-span-2">
		{#if ready.length === 0}
			<p class="text-muted-foreground px-2 py-6 text-center text-[13px]">Per-PC rates appear once jobs are chosen.</p>
		{:else}
			<div class="flex min-h-0 flex-1 flex-col"><ChartLine title="Rate of each PC, fastest first" xLabel="PC rank" yLabel="rate (M/s)" series={rateSeries} file="compare-{ids.join('-')}-pc-rates" yFmt={(v) => String(+v.toFixed(1))} /></div>
		{/if}
	</Panel>
</Page>
