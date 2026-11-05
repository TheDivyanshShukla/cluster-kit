<script lang="ts">
	import { CircleAlert, ListChecks, Unplug } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import LiveCanvas from '#lib/components/LiveCanvas.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import type { EventRow, JobRow } from '#lib/api/schema.ts';
	import HaPanel from '#lib/components/HaPanel.svelte';
	import HeadTopology from '#lib/components/HeadTopology.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import PriorityChip from '#lib/components/PriorityChip.svelte';
	import ProgressBar from '#lib/components/ProgressBar.svelte';
	import StateChip from '#lib/components/StateChip.svelte';
	import { eventLabel } from '#lib/events.ts';
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
		Promise.all([api.jobs(30), api.events(60)])
			.then(([j, e]) => {
				jobs = j.jobs;
				events = e.events;
			})
			.catch((e) => (loadError = e instanceof ApiError ? e.message : 'Could not load recent work.'))
			.finally(() => (loaded = true));
	});
	const recentJobs = $derived([...new Map([...running, ...mergeRows(jobs, Object.values(live.jobs))].map((j) => [j.id, j])).values()].sort((a, b) => +(b.state === 'running') - +(a.state === 'running') || b.id - a.id).slice(0, 30));
	const recentEvents = $derived(mergeRows(events, live.events).slice(0, 60));
	const counts = $derived({
		running: recentJobs.filter((j) => j.state === 'running').length,
		queued: recentJobs.filter((j) => j.state === 'queued').length
	});
</script>

<Page
	title="Overview"
	subtitle="The whole fleet at a glance. Updates live."
	tabs={[{ id: 'fleet', label: 'Fleet' }, { id: 'head', label: 'Head' }, { id: 'canvas', label: 'Canvas' }, { id: 'jobs', label: 'Jobs' }, { id: 'events', label: 'Events' }]}
	class="sm:grid-cols-2 sm:grid-rows-[auto_minmax(0,1fr)_minmax(0,1fr)] lg:grid-cols-[minmax(0,1.6fr)_minmax(0,1fr)]"
>
	{#if live.error && !c}
		<Panel title="Fleet" tab="fleet" class="sm:col-span-2"><EmptyState icon={CircleAlert} title="The head is not answering" body={live.error} /></Panel>
	{:else}
		<Panel title="Fleet" tab="fleet" class="max-sm:self-start sm:col-span-2" scroll={false} pad={false}>
			{#if !c}
				<div class="grid grid-cols-2 gap-3 p-3 sm:grid-cols-4" aria-busy="true">{#each [0, 1, 2, 3] as i (i)}<div class="bg-raised h-14 animate-pulse rounded-lg"></div>{/each}</div>
			{:else}
				<dl class="divide-line grid grid-cols-2 sm:grid-cols-4 sm:divide-x" aria-label="Fleet numbers">
					{#each [
						{ label: 'PCs connected', value: count(workers.length) },
						{ label: 'Threads', value: count(cores) },
						{ label: 'Working now', value: `${busy} / ${workers.length}` },
						{ label: 'Throughput', value: rate(throughput) }
					] as k (k.label)}
						<div class="px-4 py-2.5"><dt class="text-muted-foreground text-xs">{k.label}</dt><dd class="num text-xl font-semibold tracking-tight" aria-live="off">{k.value}</dd></div>
					{/each}
				</dl>
			{/if}
		</Panel>

		<Panel title="Live canvas" tab="canvas" label="Live canvas" class="sm:row-start-2" scroll={false} pad={false}>
			{#snippet actions()}<a href="/live" class="text-primary text-xs hover:underline">Open Live</a>{/snippet}
			<div class="min-h-0 flex-1 p-1"><LiveCanvas compact /></div>
		</Panel>

		<Panel title="Who holds the cluster" tab="head" class="sm:row-start-2 sm:col-start-2">
			<HeadTopology size="mini" />
			<div class="border-line mt-4 border-t pt-3"><HaPanel bare /></div>
		</Panel>

		<Panel title="Recent events" tab="events" class="sm:row-start-3 sm:col-start-1" count={recentEvents.length || undefined}>
			{#snippet actions()}<a href="/events" class="text-primary text-xs hover:underline">All events</a>{/snippet}
			{#if !loaded}
				<div class="bg-raised h-32 animate-pulse rounded-lg" aria-busy="true"></div>
			{:else if recentEvents.length === 0}
				<EmptyState icon={Unplug} title="Nothing logged yet" body="Logins, job changes and PCs joining are recorded here." />
			{:else}
				<ul class="divide-line -my-1 divide-y">
					{#each recentEvents as e (e.id)}
						<li class="py-2">
							<p class="flex items-baseline justify-between gap-2">
								<span class="text-xs font-medium" title={e.kind}>{eventLabel(e.kind)}</span>
								<span class="text-muted-foreground shrink-0 text-xs" title={datetime(e.ts_ms)}>{ago(e.ts_ms, now)}</span>
							</p>
							<p class="text-muted-foreground mt-0.5 break-words text-[13px]">{e.actor ? `${e.actor}: ` : ''}{e.detail}</p>
						</li>
					{/each}
				</ul>
			{/if}
		</Panel>

		<Panel title="Jobs" tab="jobs" class="sm:row-start-3 sm:col-start-2" count={running.length ? `${running.length} running` : undefined}>
			{#snippet actions()}<a href="/jobs" class="text-primary text-xs hover:underline">All jobs</a>{/snippet}
			{#if !loaded}
				<div class="bg-raised h-32 animate-pulse rounded-lg" aria-busy="true"></div>
			{:else if loadError && recentJobs.length === 0}
				<EmptyState icon={CircleAlert} title="Could not load jobs" body={loadError} />
			{:else if recentJobs.length === 0}
				<EmptyState icon={ListChecks} title="No job yet" body="Jobs you submit show up here with live progress. Submit one from New job or with ck run." />
			{:else}
				<ul class="divide-line -my-1 divide-y">
					{#each recentJobs as j (j.id)}
						<li>
							<a href="/jobs/{j.id}" class="hover:bg-raised -mx-3 flex flex-col gap-1.5 px-3 py-2">
								<span class="flex items-center justify-between gap-2">
									<span class="min-w-0 truncate font-medium">#{j.id} {j.name}</span><StateChip state={j.state} />
								</span>
								<ProgressBar value={progress(j)} label="Progress of job {j.id}" tone={j.state === 'failed' ? 'bg-bad' : j.state === 'done' ? 'bg-ok' : 'bg-primary'} />
								{#if j.state === 'running'}
									<span class="num text-muted-foreground text-xs">{Math.round(progress(j) * 100)}% · {rate(jobRate(j, now))}{etaMs(j, now) != null ? ` · about ${duration(etaMs(j, now))} left` : ''}</span>
								{/if}
							</a>
						</li>
					{/each}
				</ul>
			{/if}
		</Panel>
	{/if}
</Page>
