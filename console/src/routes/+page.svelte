<script lang="ts">
	import { CircleAlert, ListChecks, Unplug } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { EventRow, JobRow } from '#lib/api/schema.ts';
	import HaPanel from '#lib/components/HaPanel.svelte';
	import HeadTopology from '#lib/components/HeadTopology.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import PriorityChip from '#lib/components/PriorityChip.svelte';
	import ProgressBar from '#lib/components/ProgressBar.svelte';
	import StateChip from '#lib/components/StateChip.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { ago, count, datetime, duration, rate } from '#lib/format.ts';
	import { elapsedMs, etaMs, jobRate, mergeRows, progress, runningJobs } from '#lib/jobs.ts';
	import { card } from '#lib/ui.ts';

	const c = $derived(live.cluster);
	const workers = $derived(c?.workers ?? []);
	const cores = $derived(workers.reduce((a, w) => a + w.threads, 0));
	const busy = $derived(workers.filter((w) => w.busy).length);
	const running = $derived(runningJobs(c));
	const throughput = $derived(running.length ? workers.filter((w) => w.busy).reduce((a, w) => a + w.rate, 0) : 0);
	const now = $derived(c?.now_ms ?? Date.now());

	let jobs = $state<JobRow[]>([]);
	let events = $state<EventRow[]>([]);
	let loadError = $state<string | null>(null);
	let loaded = $state(false);
	$effect(() => {
		Promise.all([api.jobs(5), api.events(8)])
			.then(([j, e]) => {
				jobs = j.jobs;
				events = e.events;
			})
			.catch((e) => (loadError = e instanceof ApiError ? e.message : 'Could not load recent work.'))
			.finally(() => (loaded = true));
	});
	const recentJobs = $derived(mergeRows(jobs, Object.values(live.jobs)).slice(0, 5));
	const recentEvents = $derived(mergeRows(events, live.events).slice(0, 8));
	const counts = $derived({
		running: recentJobs.filter((j) => j.state === 'running').length,
		queued: recentJobs.filter((j) => j.state === 'queued').length
	});
</script>

<svelte:head><title>Overview · Cluster Kit</title></svelte:head>

<h1 class="text-xl font-semibold tracking-tight">Overview</h1>
<p class="text-muted-foreground mt-1">The whole fleet at a glance. Updates live.</p>

{#if live.error && !c}
	<div class="mt-6"><EmptyState icon={CircleAlert} title="The head is not answering" body={live.error} /></div>
{:else if !c}
	<div class="mt-6 grid grid-cols-2 gap-3 lg:grid-cols-4" aria-busy="true">
		{#each [0, 1, 2, 3] as i (i)}<div class="{card} h-[86px] animate-pulse"></div>{/each}
	</div>
{:else}
	<section class="mt-6 grid grid-cols-2 gap-3 lg:grid-cols-4" aria-label="Fleet numbers">
		{#each [
			{ label: 'PCs connected', value: count(workers.length) },
			{ label: 'Threads', value: count(cores) },
			{ label: 'Working now', value: `${busy} / ${workers.length}` },
			{ label: 'Throughput', value: rate(throughput) }
		] as k (k.label)}
			<div class="{card} px-4 py-3.5">
				<p class="text-muted-foreground text-xs">{k.label}</p>
				<p class="num mt-1 text-2xl font-semibold tracking-tight" aria-live="off">{k.value}</p>
			</div>
		{/each}
	</section>

	<div class="mt-4 grid items-start gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,22rem)]">
		<HaPanel />
		<section class="{card} px-4 py-4" aria-label="Head and standbys">
			<h2 class="mb-2 font-medium">Who holds the cluster</h2>
			<HeadTopology size="mini" />
		</section>
	</div>

	<section class="{card} mt-4 px-4 py-4" aria-label="Running jobs">
		<h2 class="font-medium">Running jobs{running.length ? ` (${running.length})` : ''}</h2>
		{#if running.length}
			<ul class="divide-line mt-2 divide-y">
				{#each running as j (j.id)}
					<li class="py-3 first:pt-1 last:pb-0">
						<div class="flex flex-wrap items-center justify-between gap-x-2 gap-y-1">
							<p class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
								<StateChip state={j.state} /><PriorityChip priority={j.priority} /><a href="/jobs/{j.id}" class="font-medium hover:underline">#{j.id} {j.name || `Job ${j.id}`}</a>
							</p>
							<span class="text-muted-foreground text-xs">{j.kind} · by {j.submitter}{j.selector ? ' · ' : ''}<code class="font-mono">{j.selector}</code></span>
						</div>
						<div class="mt-2"><ProgressBar value={progress(j)} label="Progress of job {j.id}" /></div>
						<p class="num text-muted-foreground mt-1.5 text-xs" role="status">
							{count(j.done)} of {count(j.total)} · {Math.round(progress(j) * 100)}% · {rate(jobRate(j, now))} · {duration(elapsedMs(j, now))} elapsed{etaMs(j, now) != null ? ` · about ${duration(etaMs(j, now))} left` : ''}
						</p>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="text-muted-foreground mt-2">Idle. Submit work from <a href="/jobs/new" class="text-primary hover:underline">New job</a> or with <code class="font-mono text-xs">ck run</code>.</p>
		{/if}
	</section>

	<div class="mt-4 grid gap-4 lg:grid-cols-2">
		<section aria-label="Recent jobs">
			<div class="mb-2 flex items-baseline justify-between">
				<h2 class="font-medium">Jobs</h2>
				<a href="/jobs" class="text-primary text-xs hover:underline">All jobs</a>
			</div>
			{#if !loaded}
				<div class="{card} h-40 animate-pulse" aria-busy="true"></div>
			{:else if loadError && recentJobs.length === 0}
				<EmptyState icon={CircleAlert} title="Could not load jobs" body={loadError} />
			{:else if recentJobs.length === 0}
				<EmptyState icon={ListChecks} title="No job yet" body="Jobs you submit show up here with live progress." />
			{:else}
				<p class="text-muted-foreground mb-2 text-xs">{counts.running} running · {counts.queued} queued among the latest.</p>
				<ul class="{card} divide-line divide-y">
					{#each recentJobs as j (j.id)}
						<li>
							<a href="/jobs/{j.id}" class="hover:bg-raised flex flex-col gap-1.5 px-3 py-2.5">
								<span class="flex items-center justify-between gap-2">
									<span class="min-w-0 truncate font-medium">#{j.id} {j.name}</span><StateChip state={j.state} />
								</span>
								<ProgressBar value={progress(j)} label="Progress of job {j.id}" tone={j.state === 'failed' ? 'bg-bad' : j.state === 'done' ? 'bg-ok' : 'bg-primary'} />
							</a>
						</li>
					{/each}
				</ul>
			{/if}
		</section>

		<section aria-label="Recent events">
			<div class="mb-2 flex items-baseline justify-between">
				<h2 class="font-medium">Recent events</h2>
				<a href="/events" class="text-primary text-xs hover:underline">All events</a>
			</div>
			{#if !loaded}
				<div class="{card} h-40 animate-pulse" aria-busy="true"></div>
			{:else if recentEvents.length === 0}
				<EmptyState icon={Unplug} title="Nothing logged yet" body="Logins, job changes and PCs joining are recorded here." />
			{:else}
				<ul class="{card} divide-line divide-y">
					{#each recentEvents as e (e.id)}
						<li class="px-3 py-2.5">
							<p class="flex items-baseline justify-between gap-2">
								<span class="font-mono text-xs font-medium">{e.kind}</span>
								<span class="text-muted-foreground shrink-0 text-xs" title={datetime(e.ts_ms)}>{ago(e.ts_ms, now)}</span>
							</p>
							<p class="text-muted-foreground mt-0.5 break-words text-[13px]">{e.actor ? `${e.actor}: ` : ''}{e.detail}</p>
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	</div>

	<section class="mt-6" aria-label="PCs">
		<h2 class="mb-2 font-medium">PCs</h2>
		{#if workers.length === 0}
			<EmptyState icon={Unplug} title="No PC has joined yet" body="Start an agent on any PC on this network: ck agent. It finds the head by itself." />
		{:else}
			<ul class="grid grid-cols-[repeat(auto-fill,minmax(44px,1fr))] gap-1.5">
				{#each workers as w (w.id)}
					<li
						class="flex aspect-square items-center justify-center rounded-md border text-[10px] font-medium {w.busy
							? 'border-busy bg-busy/15 text-foreground'
							: 'border-line bg-surface text-muted-foreground'}"
						title="{w.host ?? w.addr} · {w.busy ? 'working' : 'idle'} · {rate(w.rate)}"
					>
						<span class="sr-only">{w.host ?? w.addr}: {w.busy ? 'working' : 'idle'}</span>
						<span aria-hidden="true">{w.busy ? '●' : '○'}</span>
					</li>
				{/each}
			</ul>
		{/if}
	</section>
{/if}
