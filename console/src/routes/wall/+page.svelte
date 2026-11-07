<script lang="ts">
	import { Moon, RotateCcw, Sun, Sunrise, TriangleAlert, User } from '@lucide/svelte';
	import { api } from '#lib/api/client.ts';
	import LiveCanvas from '#lib/components/LiveCanvas.svelte';
	import HeadTopology from '#lib/components/HeadTopology.svelte';
	import { count, duration, rate, watts } from '#lib/format.ts';
	import { progress, runningJobs } from '#lib/jobs.ts';
	import { failoverView } from '#lib/ha.ts';
	import { live, loadPowerCfg, pcsOnline, tickFailover } from '#lib/live.svelte.ts';
	import { toItems } from '#lib/map.ts';
	import { powerCounts, tickerLines } from '#lib/topology.ts';

	// Wall mode (UI-11): no controls, read from 5 m. Sizes are in vw so the text scales with the projector.
	let now = $state(Date.now());
	$effect(() => {
		const t = setInterval(() => {
			now = Date.now();
			tickFailover(now);
		}, 1000);
		return () => clearInterval(t);
	});
	$effect(() => void loadPowerCfg());
	// the ticker also shows what happened just before this page opened
	$effect(() => {
		api.events(40).then((r) => {
			const seen = new Set(live.events.map((e) => e.id));
			live.events = [...live.events, ...r.events.filter((e) => !seen.has(e.id))].sort((a, b) => b.id - a.id).slice(0, 100);
		}).catch(() => {});
	});
	const pw = $derived(powerCounts(Object.values(live.nodes)));
	const allAwake = $derived(live.powerCfg?.mode === 'off' && pw.asleep === 0 && pw.waking === 0);
	const ticker = $derived(tickerLines(live.events, now));
	const ws = $derived(live.cluster?.workers ?? []);
	const items = $derived(toItems(live.nodes, ws));
	const jobs = $derived(runningJobs(live.cluster));
	const inUse = $derived(ws.filter((w) => w.presence === 'busy').length);
	const fo = $derived(failoverView(live.failover, now));
	const lost = $derived(Object.values(live.lost));
	const stats = $derived([
		{ label: 'PCs online', value: count(pcsOnline(ws)) },
		{ label: 'Threads', value: count(ws.reduce((a, w) => a + w.threads, 0)) },
		{ label: 'Throughput', value: rate(ws.reduce((a, w) => a + w.rate, 0)) }
	]);
</script>

<svelte:head><title>Wall · Cluster Kit</title></svelte:head>

<main class="bg-background text-foreground flex h-dvh flex-col gap-[1.5vh] overflow-hidden p-[1.4vw]">
	<h1 class="sr-only">Cluster wall</h1>
	<a href="/" class="bg-primary text-primary-foreground sr-only rounded-lg px-3 py-2 focus:not-sr-only focus:absolute focus:top-2 focus:left-2 focus:z-50">Leave wall mode</a>
	<div class="flex flex-col gap-[1vw] lg:flex-row">
	<div class="grid min-w-0 flex-1 grid-cols-2 gap-[1vw] md:grid-cols-4 lg:grid-cols-2">
		{#each stats as s (s.label)}
			<div class="bg-surface border-line rounded-lg border px-[1.2vw] py-[1vh]">
				<p class="text-muted-foreground text-[max(0.9rem,1.3vw)]">{s.label}</p>
				<p class="num text-[max(2rem,4.6vw)] lg:text-[3.3vw] leading-none font-semibold tracking-tight">{s.value}</p>
			</div>
		{/each}
		<div class="bg-surface border-line min-w-0 rounded-lg border px-[1.2vw] py-[1vh]">
			<p class="text-muted-foreground text-[max(0.9rem,1.3vw)]">{jobs.length > 1 ? `${jobs.length} running jobs` : 'Current job'}</p>
			{#if jobs.length === 0}
				<p class="text-muted-foreground text-[max(1.5rem,3vw)] leading-none font-semibold">Idle</p>
			{:else if jobs.length === 1}
				{@const pj = Math.round(progress(jobs[0]) * 100)}
				<p class="num text-[max(2rem,4.6vw)] lg:text-[3.3vw] leading-none font-semibold tracking-tight">{pj}%</p>
				<p class="text-muted-foreground truncate text-[max(0.8rem,1vw)]">#{jobs[0].id}{jobs[0].name ? ` · ${jobs[0].name}` : ''}</p>
			{/if}
			{#each jobs.length > 1 ? jobs.slice(0, 3) : jobs as j (j.id)}
				{@const pj = Math.round(progress(j) * 100)}
				<div class={jobs.length > 1 ? 'mt-[0.6vh]' : 'mt-[0.8vh]'}>
					{#if jobs.length > 1}
						<p class="flex items-baseline justify-between gap-2 text-[max(0.8rem,1.05vw)] leading-tight"><span class="truncate">#{j.id} {j.name}</span><span class="num font-semibold">{pj}%</span></p>
					{/if}
					<div class="bg-raised h-[1.2vh] rounded-full" role="progressbar" aria-label="Progress of job {j.id}" aria-valuenow={pj} aria-valuemin="0" aria-valuemax="100">
						<div class="bg-primary h-full rounded-full transition-[width] duration-700" style="width:{pj}%"></div>
					</div>
				</div>
			{/each}
			{#if jobs.length > 3}<p class="text-muted-foreground text-[max(0.8rem,1vw)]">+{jobs.length - 3} more</p>{/if}
		</div>
	</div>
	<div class="bg-surface border-line shrink-0 rounded-lg border p-[0.9vw] max-lg:p-3 lg:w-[32vw] lg:max-w-[480px]"><HeadTopology size="wall" /></div>
	</div>

	<div class="flex flex-wrap items-center gap-x-[2vw] gap-y-1 text-[max(1rem,1.5vw)] font-semibold" role="status" aria-label="PCs by power state">
		{#if allAwake}
			<span class="text-ok flex items-center gap-[0.6vw]"><Sun class="size-[1.8vw] min-h-5 min-w-5" aria-hidden="true" /><span>All PCs awake</span></span>
		{:else}
			<span class="text-muted-foreground flex items-center gap-[0.6vw]"><Moon class="size-[1.8vw] min-h-5 min-w-5" aria-hidden="true" /><span class="num">{pw.asleep}</span> asleep</span>
			<span class="text-ok flex items-center gap-[0.6vw]"><Sun class="size-[1.8vw] min-h-5 min-w-5" aria-hidden="true" /><span class="num">{pw.awake}</span> awake</span>
			<span class="text-warn flex items-center gap-[0.6vw]"><Sunrise class="size-[1.8vw] min-h-5 min-w-5" aria-hidden="true" /><span class="num">{pw.waking}</span> waking</span>
		{/if}
		{#if live.labPower.length}<span class="text-muted-foreground num" title={live.labPower.map((l) => `${l.lab} ${watts(l.watts)}`).join(', ')}>{watts(live.labPower.reduce((a, l) => a + l.watts, 0))} drawn</span>{/if}
	</div>

	{#if fo?.kind === 'down'}
		<div class="border-warn bg-warn/15 flex items-center gap-[1vw] rounded-lg border-2 px-[1.4vw] py-[1vh] text-[max(1rem,1.9vw)] font-semibold" role="alert">
			<TriangleAlert class="size-[2.4vw] min-h-6 min-w-6 shrink-0" aria-hidden="true" />
			<p>Head failed — standby taking over (<span class="num">{fo.secs} s</span>)</p>
		</div>
	{:else if fo?.kind === 'recovered'}
		<div class="border-ok bg-ok/10 flex items-center gap-[1vw] rounded-lg border px-[1.4vw] py-[0.8vh] text-[max(1rem,1.7vw)] font-semibold" role="status">
			<RotateCcw class="size-[2.2vw] min-h-5 min-w-5 shrink-0" aria-hidden="true" />
			<p>Recovered in <span class="num">{fo.secs} s</span></p>
		</div>
	{/if}

	{#if inUse > 0}
		<div class="border-busy bg-busy/10 flex items-center gap-[1vw] rounded-lg border px-[1.4vw] py-[0.8vh] text-[max(1rem,1.7vw)] font-semibold" role="status">
			<User class="size-[2.2vw] min-h-5 min-w-5 shrink-0" aria-hidden="true" />
			<p><span class="num">{inUse}</span> {inUse === 1 ? 'PC' : 'PCs'} in use by students</p>
		</div>
	{/if}

	{#if lost.length}
		<div class="border-bad bg-bad/15 flex items-center gap-[1vw] rounded-lg border-2 px-[1.4vw] py-[1vh] text-[max(1rem,1.9vw)] font-semibold" role="alert">
			<TriangleAlert class="size-[2.4vw] min-h-6 min-w-6 shrink-0" aria-hidden="true" />
			<p>
				{lost.map((l) => l.host).join(', ')} lost. Recovering its work
				<span class="num ml-[0.6vw]">{lost.map((l) => duration(now - l.at)).join(' · ')}</span>
			</p>
		</div>
	{/if}

	{#if items.length > 0}
		<div class="bg-surface border-line min-h-[260px] flex-1 rounded-lg border p-[0.4vw]"><LiveCanvas wall /></div>
	{:else if live.labs.length === 0}
		<p class="text-muted-foreground m-auto text-[max(1rem,2vw)]">No lab map yet. Create one under Floor map.</p>
	{/if}
	{#if ticker.length}
		<ul class="text-muted-foreground shrink-0 text-[max(0.85rem,1.1vw)]" aria-label="Recent head and power events">
			{#each ticker as t (t.id)}<li class="truncate transition-opacity duration-700" style="opacity:{t.fade}">{t.text}</li>{/each}
		</ul>
	{/if}
</main>
