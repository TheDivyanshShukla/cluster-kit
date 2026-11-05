<script lang="ts">
	import { CircleAlert, ListChecks, Plus } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { JobRow } from '#lib/api/schema.ts';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import PriorityChip from '#lib/components/PriorityChip.svelte';
	import ProgressBar from '#lib/components/ProgressBar.svelte';
	import StateChip from '#lib/components/StateChip.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { ago, count, datetime } from '#lib/format.ts';
	import { atLeast, mergeRows, progress } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnGhost, btnPrimary, card } from '#lib/ui.ts';

	let items = $state<JobRow[]>([]);
	let next = $state<number | null>(null);
	let loading = $state(true);
	let more = $state(false);
	let error = $state<string | null>(null);

	async function load(before?: number | null) {
		try {
			const r = await api.jobs(50, before);
			items = before == null ? r.jobs : [...items, ...r.jobs];
			next = r.next;
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load jobs.';
		}
		loading = more = false;
	}
	$effect(() => void load());

	const rows = $derived(mergeRows(items, Object.values(live.jobs)));
	const now = $derived(live.cluster?.now_ms ?? Date.now());
</script>

<svelte:head><title>Jobs · Cluster Kit</title></svelte:head>

<div class="flex flex-wrap items-end justify-between gap-3">
	<div>
		<h1 class="text-xl font-semibold tracking-tight">Jobs</h1>
		<p class="text-muted-foreground mt-1">Everything submitted to the cluster, newest first.</p>
	</div>
	{#if atLeast(roleNow(), 'researcher')}
		<a href="/jobs/new" class={btnPrimary}><Plus class="size-4" aria-hidden="true" />New job</a>
	{/if}
</div>

<div class="mt-5">
	{#if loading}
		<div class="{card} h-48 animate-pulse" aria-busy="true"></div>
	{:else if error && rows.length === 0}
		<EmptyState icon={CircleAlert} title="Could not load jobs" body={error}>
			{#snippet action()}<button type="button" class={btnGhost} onclick={() => ((loading = true), load())}>Try again</button>{/snippet}
		</EmptyState>
	{:else if rows.length === 0}
		<EmptyState icon={ListChecks} title="No job has been submitted" body="Submit one here or from a terminal with ck run. It appears here with live progress.">
			{#snippet action()}
				{#if atLeast(roleNow(), 'researcher')}<a href="/jobs/new" class={btnPrimary}><Plus class="size-4" aria-hidden="true" />New job</a>{/if}
			{/snippet}
		</EmptyState>
	{:else}
		<div class="{card} relative overflow-x-auto">
			<table class="w-full text-left text-[13px]">
				<thead class="text-muted-foreground border-line border-b text-xs">
					<tr>
						<th class="px-3 py-2 font-medium">Job</th>
						<th class="px-3 py-2 font-medium">State</th>
						<th class="px-3 py-2 font-medium">Priority</th>
						<th class="px-3 py-2 font-medium">Run on</th>
						<th class="min-w-36 px-3 py-2 font-medium">Progress</th>
						<th class="px-3 py-2 font-medium">Kind</th>
						<th class="px-3 py-2 font-medium">By</th>
						<th class="px-3 py-2 text-right font-medium">Numbers</th>
						<th class="px-3 py-2 font-medium">Submitted</th>
					</tr>
				</thead>
				<tbody>
					{#each rows as j (j.id)}
						<tr class="border-line hover:bg-raised border-b last:border-0">
							<td class="min-w-48 px-3 py-2">
								<a href="/jobs/{j.id}" class="font-medium hover:underline"><span class="num text-muted-foreground">#{j.id}</span> {j.name || 'Untitled'}</a>
							</td>
							<td class="px-3 py-2"><StateChip state={j.state} /></td>
							<td class="px-3 py-2"><PriorityChip priority={j.priority} /></td>
							<td class="max-w-48 truncate px-3 py-2 font-mono text-xs {j.selector ? '' : 'text-muted-foreground'}" title={j.selector}>{j.selector || 'any PC'}</td>
							<td class="px-3 py-2">
								<div class="flex items-center gap-2">
									<div class="min-w-20 flex-1"><ProgressBar value={progress(j)} label="Progress of job {j.id}" tone={j.state === 'failed' ? 'bg-bad' : j.state === 'done' ? 'bg-ok' : 'bg-primary'} /></div>
									<span class="num text-muted-foreground w-9 text-right text-xs">{Math.round(progress(j) * 100)}%</span>
								</div>
							</td>
							<td class="text-muted-foreground px-3 py-2 font-mono text-xs">{j.kind}</td>
							<td class="px-3 py-2">{j.submitter}</td>
							<td class="num px-3 py-2 text-right">{count(j.total)}</td>
							<td class="text-muted-foreground px-3 py-2 whitespace-nowrap" title={datetime(j.created_ms)}>{ago(j.created_ms, now)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		{#if error}<p class="text-bad mt-3 text-[13px]" role="alert">{error}</p>{/if}
		{#if next != null}
			<div class="mt-4 flex justify-center">
				<button type="button" class={btnGhost} disabled={more} onclick={() => ((more = true), load(next))}>{more ? 'Loading…' : 'Load older jobs'}</button>
			</div>
		{/if}
	{/if}
</div>
