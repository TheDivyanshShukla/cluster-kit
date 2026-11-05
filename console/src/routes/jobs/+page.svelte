<script lang="ts">
	import { CircleAlert, ExternalLink, ListChecks, Plus } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { JobRow } from '#lib/api/schema.ts';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import PriorityChip from '#lib/components/PriorityChip.svelte';
	import ProgressBar from '#lib/components/ProgressBar.svelte';
	import StateChip from '#lib/components/StateChip.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { ago, count, datetime, duration, rate } from '#lib/format.ts';
	import { atLeast, canCancel, elapsedMs, etaMs, jobRate, mergeRows, progress } from '#lib/jobs.ts';
	import { Listing } from '#lib/listing.svelte.ts';
	import type { Spec } from '#lib/listing.ts';
	import { priorityLabel } from '#lib/org.ts';
	import { roleNow, userNow } from '#lib/session.svelte.ts';
	import { setQuery } from '#lib/urlstate.svelte.ts';
	import { isPhone, vp } from '#lib/viewport.svelte.ts';
	import { btnDanger, btnGhost, btnPrimary } from '#lib/ui.ts';

	let items = $state<JobRow[]>([]);
	let next = $state<number | null>(null);
	let loading = $state(true);
	let more = false;
	let error = $state<string | null>(null);
	let focus = $state<number | null>(null);
	let cancelErr = $state('');
	let ask = $state(false);
	let cancelling = $state(false);

	async function load(before?: number | null) {
		more = true;
		try {
			const r = await api.jobs(200, before);
			items = before == null ? r.jobs : [...items, ...r.jobs];
			next = r.next;
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load jobs.';
		}
		loading = more = false;
	}
	$effect(() => void load());
	const older = () => next != null && !more && void load(next);

	const rows0 = $derived(mergeRows(items, Object.values(live.jobs)));
	const now = $derived(live.cluster?.now_ms ?? Date.now());
	const PRIOS = [0, 1, 2, 3, 4].map(priorityLabel);
	const STATES = ['running', 'queued', 'done', 'failed', 'cancelled'];
	// running first, then queued, then newest
	const spec: Spec<JobRow> = {
		facets: [
			{ key: 'state', label: 'State', get: (j) => j.state, order: STATES },
			{ key: 'kind', label: 'Kind', get: (j) => j.kind },
			{ key: 'priority', label: 'Priority', get: (j) => priorityLabel(j.priority), order: PRIOS },
			{ key: 'by', label: 'By', get: (j) => j.submitter }
		],
		text: (j) => [j.id, j.name, j.submitter, j.selector, j.kind, j.state],
		sorters: {
			id: (j) => j.id,
			state: (j) => STATES.indexOf(j.state),
			priority: (j) => j.priority,
			progress: (j) => progress(j),
			kind: (j) => j.kind,
			by: (j) => j.submitter,
			total: (j) => j.total,
			created: (j) => j.created_ms
		},
		base: (a, b) => STATES.indexOf(a.state) - STATES.indexOf(b.state) || b.id - a.id
	};
	const l = new Listing<JobRow>(spec);
	const res = $derived(l.run(rows0));
	const cur = $derived(focus != null ? live.jobs[focus] ?? rows0.find((j) => j.id === focus) : undefined);

	const COLS: (Col & { min?: number })[] = [
		{ key: 'id', label: 'Job', w: 'minmax(110px,2fr)', sort: true },
		{ key: 'state', label: 'State', w: '104px', sort: true },
		{ key: 'priority', label: 'Priority', w: '110px', sort: true, min: 900 },
		{ key: 'progress', label: 'Progress', w: 'minmax(104px,1fr)', sort: true },
		{ key: 'kind', label: 'Kind', w: '64px', sort: true, min: 1100 },
		{ key: 'by', label: 'By', w: '84px', sort: true, min: 700 },
		{ key: 'total', label: 'Numbers', w: '150px', right: true, sort: true, min: 1250 },
		{ key: 'created', label: 'Submitted', w: '120px', sort: true, min: 700 }
	];
	const cols = $derived<Col[]>(COLS.filter((c) => vp.w >= (c.min ?? 0)));
	const open = (j: JobRow) => ((focus = j.id), isPhone() && setQuery('tab', 'detail'));
	async function cancel(id: number) {
		cancelErr = '';
		cancelling = true;
		try {
			live.jobs[id] = (await api.cancelJob(id)).job;
		} catch (e) {
			cancelErr = e instanceof ApiError ? e.message : 'Could not cancel the job.';
		}
		cancelling = false;
		ask = false;
	}
</script>

<Page
	title="Jobs"
	subtitle="Everything submitted to the cluster. Running jobs first, then newest."
	tabs={[{ id: 'list', label: 'List', badge: res.rows.length }, { id: 'detail', label: 'Detail' }]}
	class="sm:grid-cols-[minmax(0,1fr)_17rem] lg:grid-cols-[minmax(0,1fr)_22rem]"
>
	{#snippet actions()}
		{#if atLeast(roleNow(), 'researcher')}<a href="/jobs/new" class={btnPrimary}><Plus class="size-4" aria-hidden="true" />New job</a>{/if}
	{/snippet}

	<Panel title="List" tab="list" label="Jobs" count={rows0.length} scroll={false} pad={false}>
		{#snippet toolbar()}
			<ListToolbar {l} rows={rows0} shown={res.rows.length} total={rows0.length} placeholder="Search jobs" />
		{/snippet}
		{#if loading}
			<div class="bg-raised m-3 h-40 animate-pulse rounded-lg" aria-busy="true"></div>
		{:else if error && rows0.length === 0}
			<div class="p-4">
				<EmptyState icon={CircleAlert} title="Could not load jobs" body={error}>
					{#snippet action()}<button type="button" class={btnGhost} onclick={() => ((loading = true), load())}>Try again</button>{/snippet}
				</EmptyState>
			</div>
		{:else}
			<DataTable
				label="Jobs"
				rows={res.rows}
				{cols}
				rowKey={(j) => j.id}
				sort={l.state.sort}
				dir={l.state.dir}
				onsort={(c) => l.sortBy(c)}
				active={(j) => j.id === focus}
				onrow={open}
				onend={older}
				minW={0}
			>
				{#snippet cell(j, c)}
					{#if c.key === 'id'}
						<a href="/jobs/{j.id}" class="block truncate font-medium hover:underline" title={j.name}><span class="num text-muted-foreground">#{j.id}</span> <Hl text={j.name || 'Untitled'} q={l.state.q} /></a>
					{:else if c.key === 'state'}<StateChip state={j.state} />
					{:else if c.key === 'priority'}<PriorityChip priority={j.priority} />
					{:else if c.key === 'progress'}
						<div class="flex items-center gap-2">
							<div class="min-w-12 flex-1"><ProgressBar value={progress(j)} label="Progress of job {j.id}" tone={j.state === 'failed' ? 'bg-bad' : j.state === 'done' ? 'bg-ok' : 'bg-primary'} /></div>
							<span class="num text-muted-foreground w-9 text-right text-xs">{Math.round(progress(j) * 100)}%</span>
						</div>
					{:else if c.key === 'kind'}<span class="text-muted-foreground font-mono text-xs">{j.kind}</span>
					{:else if c.key === 'by'}<span class="block truncate"><Hl text={j.submitter} q={l.state.q} /></span>
					{:else if c.key === 'total'}<span class="num">{count(j.total)}</span>
					{:else if c.key === 'created'}<span class="text-muted-foreground whitespace-nowrap" title={datetime(j.created_ms)}>{ago(j.created_ms, now)}</span>
					{/if}
				{/snippet}
				{#snippet empty()}
					<div class="p-4">
						{#if rows0.length === 0}
							<EmptyState icon={ListChecks} title="No job has been submitted" body="Submit one here or from a terminal with ck run. It appears here with live progress." />
						{:else}
							<EmptyState icon={ListChecks} title="No job matches these filters" body="Nothing fits the search and chips you chose.">
								{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
							</EmptyState>
						{/if}
					</div>
				{/snippet}
			</DataTable>
		{/if}
	</Panel>

	<Panel title="Detail" tab="detail" label="Selected job">
		{#if cur}
			<div class="space-y-3 text-[13px]">
				<div>
					<p class="text-[15px] font-semibold break-words"><span class="num text-muted-foreground">#{cur.id}</span> {cur.name || 'Untitled job'}</p>
					<p class="mt-1.5 flex flex-wrap items-center gap-2"><StateChip state={cur.state} /><PriorityChip priority={cur.priority} /></p>
				</div>
				<ProgressBar value={progress(cur)} label="Job progress" tone={cur.state === 'failed' ? 'bg-bad' : cur.state === 'done' ? 'bg-ok' : 'bg-primary'} />
				<p class="num text-muted-foreground text-xs">
					{count(cur.done)} of {count(cur.total)} · {Math.round(progress(cur) * 100)}%{cur.state === 'running' && etaMs(cur, now) != null ? ` · about ${duration(etaMs(cur, now))} left` : ''}
				</p>
				{#if cur.state === 'failed'}<p class="text-bad flex items-start gap-1.5" role="alert"><CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" /><span class="break-words">{cur.error ?? 'no reason was recorded.'}</span></p>{/if}
				<dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5">
					<dt class="text-muted-foreground">Kind</dt><dd>{cur.kind}</dd>
					<dt class="text-muted-foreground">Run on</dt><dd class="font-mono text-xs break-all">{cur.selector || 'any PC'}</dd>
					<dt class="text-muted-foreground">By</dt><dd>{cur.submitter}</dd>
					<dt class="text-muted-foreground">Submitted</dt><dd>{datetime(cur.created_ms)}</dd>
					<dt class="text-muted-foreground">Elapsed</dt><dd class="num">{duration(elapsedMs(cur, now))}</dd>
					<dt class="text-muted-foreground">Rate</dt><dd class="num">{rate(jobRate(cur, now))}</dd>
				</dl>
				{#if cancelErr}<p class="text-bad" role="alert">{cancelErr}</p>{/if}
				<div class="flex flex-wrap gap-2">
					<a href="/jobs/{cur.id}" class={btnGhost}><ExternalLink class="size-4" aria-hidden="true" />Open parts and result</a>
					{#if canCancel(roleNow(), userNow(), cur)}<button type="button" class={btnDanger} onclick={() => (ask = true)}>Cancel job</button>{/if}
				</div>
			</div>
		{:else}
			<p class="text-muted-foreground text-[13px]">Click a job to see its progress and numbers here.</p>
		{/if}
	</Panel>
</Page>

{#if cur}
	<ConfirmDialog bind:open={ask} title="Cancel job {cur.id}?" body="Work already done is kept, but the rest will not run. This cannot be undone." confirm="Cancel job" busy={cancelling} onconfirm={() => cancel(cur.id)} />
{/if}
