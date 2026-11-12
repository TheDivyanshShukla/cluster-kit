<script lang="ts">
	import { page } from '$app/state';
	import { ArrowLeft, CircleAlert, Clock, Download, FileDown, FileQuestion, GitCompareArrows, Square } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Breakdown, JobFile } from '#lib/api/schema.ts';
	import BreakdownPanel from '#lib/components/BreakdownPanel.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import Listbox from '#lib/components/Listbox.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import PriorityChip from '#lib/components/PriorityChip.svelte';
	import ProgressBar from '#lib/components/ProgressBar.svelte';
	import StateChip from '#lib/components/StateChip.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { count, datetime, duration, fileSize, rate } from '#lib/format.ts';
	import { canCancel, elapsedMs, etaMs, jobRate, progress } from '#lib/jobs.ts';
	import { etaLeft, joinReducer, PLACEMENTS, POLICIES, reducerLabel, REDUCER_OPTIONS, splitReducer } from '#lib/sched.ts';
	import { roleNow, userNow } from '#lib/session.svelte.ts';
	import { btnDanger, btnGhost, card, field } from '#lib/ui.ts';

	const WAIT: Record<string, string> = {
		quota: 'waiting: you are at your quota',
		priority: 'waiting: higher priority jobs hold the PCs',
		no_pcs: 'waiting: no free PC (PCs someone is using are skipped)',
		'': 'waiting for its turn'
	};
	const id = $derived(Number(page.params.id));
	const SHOW = 100_000; // characters of result shown inline; the download has all of it

	let fetched = $state(false);
	let parts = $state<number | undefined>();
	let error = $state<{ message: string; status: number } | null>(null);
	let result = $state<string | null>(null);
	let resultError = $state<string | null>(null);
	let confirmOpen = $state(false);
	let cancelling = $state(false);
	let cancelError = $state<string | null>(null);
	let asked = ''; // "id|reducer" whose result was already requested
	let redId = $state('');
	let redN = $state<number | null>(5);
	let seeded = -1; // job whose own reducer filled the switch

	// live.jobs holds the newest copy of every job: the stream, the cluster tick and this page's fetches all write to it.
	let hints = $state<{ max_pcs: number; placement: string; mem_mb: number }>({ max_pcs: 0, placement: '', mem_mb: 0 });
	const job = $derived(Number.isInteger(id) ? live.jobs[id] : undefined);
	const now = $derived(live.cluster?.now_ms ?? Date.now());

	$effect(() => {
		const jid = id;
		fetched = false;
		asked = '';
		error = null;
		result = null;
		resultError = null;
		if (!Number.isInteger(jid)) return void (fetched = true);
		api.job(jid)
			.then((r) => {
				live.jobs[jid] = r.job;
				parts = r.parts;
				hints = r.job;
			})
			.catch((e) => (error = e instanceof ApiError ? { message: e.message, status: e.status } : { message: 'Could not load the job.', status: 0 }))
			.finally(() => (fetched = true));
	});

	// Where the time went; a job that never started has none (404).
	let bd = $state<Breakdown | null>(null);
	let bdState = $state<'loading' | 'ok' | 'none' | 'error'>('loading');
	let bdKey = '';
	$effect(() => {
		const jid = id, st = job?.state;
		if (!st || st === 'queued') return void (bdState = 'none');
		// refetched when the job changes state (running -> done), not on every tick
		const key = `${jid}|${st}`;
		if (key === bdKey) return;
		bdKey = key;
		if (!bd || bd.job !== jid) ((bd = null), (bdState = 'loading'));
		api.breakdown(jid)
			.then((r) => ((bd = r.breakdown), (bdState = 'ok')))
			.catch((e) => (bdState = e instanceof ApiError && e.status === 404 ? 'none' : 'error'));
	});

	// while running, every `job` event on the stream refreshes the breakdown (at most once per 2 s)
	let bdAt = 0;
	$effect(() => {
		const jid = id, j = live.jobs[id];
		if (!j || j.state !== 'running') return;
		const t = setTimeout(() => {
			bdAt = Date.now();
			api.breakdown(jid).then((r) => ((bd = r.breakdown), (bdState = 'ok'))).catch(() => {});
		}, Math.max(0, 2000 - (Date.now() - bdAt)));
		return () => clearTimeout(t);
	});

	// Output files written by the job's commands (JOB-08); refetched when the file count changes
	let files = $state<JobFile[]>([]);
	let filesError = $state('');
	$effect(() => {
		const jid = id, n = job?.files ?? 0;
		if (!n) return void (files = []);
		api.jobFiles(jid).then((r) => ((files = r.files), (filesError = ''))).catch((e) => (filesError = e instanceof ApiError ? e.message : 'Could not list the files.'));
	});
	const hasFiles = $derived((job?.files ?? 0) > 0);

	// The switch starts on the reducer the job was submitted with.
	$effect(() => {
		if (!job || seeded === id) return;
		seeded = id;
		const r = splitReducer(job.reducer);
		redId = r.id;
		redN = r.n ?? 5;
	});
	const reducer = $derived(joinReducer(redId, redN));
	const etaWords = $derived(job?.state === 'running' ? etaLeft(job.eta_ms, now) : null);

	// A finished job: read the stored result; again whenever the reducer switch changes.
	$effect(() => {
		const jid = id, red = reducer.value;
		if (job?.state !== 'done' || seeded !== jid || red === undefined) return;
		const key = `${jid}|${red}`;
		if (asked === key) return;
		asked = key;
		resultError = null;
		api.jobResult(jid, red === job.reducer ? undefined : red)
			.then((t) => (result = t))
			.catch((e) => (resultError = e instanceof ApiError ? e.message : 'Could not read the result.'));
	});

	const blobUrl = $derived(result === null ? null : URL.createObjectURL(new Blob([result], { type: 'text/plain' })));
	$effect(() => {
		const u = blobUrl;
		return () => {
			if (u) URL.revokeObjectURL(u);
		};
	});

	async function cancel() {
		cancelling = true;
		cancelError = null;
		try {
			live.jobs[id] = (await api.cancelJob(id)).job;
			confirmOpen = false;
		} catch (e) {
			cancelError = e instanceof ApiError ? e.message : 'Could not cancel the job.';
			confirmOpen = false;
		}
		cancelling = false;
	}

	const facts = $derived(
		job
			? [
					['Kind', job.kind],
					['Run on', job.selector || 'Any PC'],
					['Range', `${count(job.from)} to ${count(job.end - 1)}`],
					['Total numbers', count(job.total)],
					['Chunk size', job.chunk > 0 ? count(job.chunk) : 'Chosen by the head'],
					['Submitted by', job.submitter],
					['Parts stored', parts == null ? '—' : count(parts)],
					['Created', datetime(job.created_ms)],
					['Started', datetime(job.started_ms)],
					['Finished', datetime(job.finished_ms)],
					['Elapsed', duration(elapsedMs(job, now))],
					['Average rate', rate(jobRate(job, now))],
					['Policy', POLICIES.find((p) => p.value === job.policy)?.label.replace(' (recommended)', '') ?? (job.policy || '—')],
					['Use at most', hints.max_pcs > 0 ? `${hints.max_pcs} PC${hints.max_pcs === 1 ? '' : 's'}` : 'All PCs'],
					['Placement', PLACEMENTS.find((p) => p.value === hints.placement)?.label ?? 'Spread'],
					...(hints.mem_mb > 0 ? [['Memory per copy', `${count(hints.mem_mb)} MB`]] : []),
					...(job.files > 0 ? [['Output files', count(job.files)]] : []),
					['Reducer', job.reducer ? reducerLabel(job.reducer) : 'Raw parts'],
					['Time left', job.state === 'running' ? (etaWords?.replace(' left', '') ?? duration(etaMs(job, now))) : '—']
				]
			: []
	);
</script>

<Page
	title={job ? `#${job.id} ${job.name || 'Untitled job'}` : 'Job'}
	tabs={[{ id: 'job', label: 'Job' }, { id: 'time', label: 'Time' }, { id: 'result', label: 'Result' }, ...(hasFiles ? [{ id: 'files', label: 'Files', badge: job?.files }] : [])]}
	class="sm:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)] {hasFiles ? 'sm:grid-rows-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)]' : 'sm:grid-rows-[minmax(0,1fr)_minmax(0,1fr)]'}"
>
	{#snippet actions()}
		{#if job && job.state !== 'queued'}<a href="/compare?jobs={job.id}" class={btnGhost}><GitCompareArrows class="size-4" aria-hidden="true" />Compare with...</a>{/if}
		{#if job}<StateChip state={job.state} /><PriorityChip priority={job.priority} />{/if}
		{#if job && canCancel(roleNow(), userNow(), job)}
			<button type="button" class={btnDanger} onclick={() => (confirmOpen = true)}><Square class="size-4" aria-hidden="true" />Cancel job</button>
		{/if}
		<a href="/jobs" class={btnGhost}><ArrowLeft class="size-4" aria-hidden="true" />All jobs</a>
	{/snippet}

	{#if !job && !fetched}
		<Panel title="Job" tab="job" class="sm:col-span-2 sm:row-span-2"><div class="bg-raised h-40 animate-pulse rounded-lg" aria-busy="true"></div></Panel>
	{:else if !job}
		<Panel title="Job" tab="job" class="sm:col-span-2 sm:row-span-2">
			{#if error && error.status !== 404}
				<EmptyState icon={CircleAlert} title="Could not load this job" body={error.message} />
			{:else}
				<EmptyState icon={FileQuestion} title="No such job" body="Job {page.params.id} does not exist. It may have been mistyped.">
					{#snippet action()}<a href="/jobs" class={btnGhost}>See all jobs</a>{/snippet}
				</EmptyState>
			{/if}
		</Panel>
	{:else}
		<Panel title="Progress and facts" tab="job" class={hasFiles ? 'sm:row-span-3' : 'sm:row-span-2'}>
			{#if cancelError}<p class="text-bad mb-3 flex items-center gap-1.5 text-[13px]" role="alert"><CircleAlert class="size-4 shrink-0" aria-hidden="true" />{cancelError}</p>{/if}
			{#if job.state === 'failed'}
				<p class="border-bad text-bad mb-3 flex items-start gap-2 rounded-lg border px-3 py-2.5" role="alert">
					<CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" /><span class="break-words">Failed: {job.error ?? 'no reason was recorded.'}</span>
				</p>
			{/if}
			<ProgressBar value={progress(job)} label="Job progress" tone={job.state === 'failed' ? 'bg-bad' : job.state === 'done' ? 'bg-ok' : 'bg-primary'} />
			<p class="num text-muted-foreground mt-2 text-xs" aria-live="polite">
				{count(job.done)} of {count(job.total)} numbers · {Math.round(progress(job) * 100)}%{job.state === 'queued' || job.wait_reason ? ` · ${WAIT[job.wait_reason ?? ''] ?? 'waiting for its turn'}` : ''}
			</p>
			{#if etaWords}<p class="mt-1 flex items-center gap-1.5 text-[13px] font-medium" aria-live="polite"><Clock class="text-muted-foreground size-4" aria-hidden="true" />{etaWords[0].toUpperCase() + etaWords.slice(1)}</p>{/if}
			<dl class="border-line mt-4 grid grid-cols-2 gap-px overflow-hidden rounded-lg border bg-[var(--line)]">
				{#each facts as [k, v] (k)}
					<div class="bg-surface px-3 py-2">
						<dt class="text-muted-foreground text-xs">{k}</dt>
						<dd class="num mt-0.5 text-[13px] break-words">{v}</dd>
					</div>
				{/each}
			</dl>
		</Panel>

		<Panel title="Where the time went" tab="time" scroll={false} pad={false}>
			{#if bdState === 'ok' && bd && bd.per_pc.length === 0 && job.state === 'running'}
				<p class="text-muted-foreground px-4 py-6 text-center">No PC has run a part yet{job.wait_reason ? `: ${WAIT[job.wait_reason]}` : ''}. The breakdown fills in as parts finish.</p>
			{:else if bdState === 'ok' && bd}
				<BreakdownPanel b={bd} />
			{:else if bdState === 'loading'}
				<div class="bg-raised m-3 h-32 animate-pulse rounded-lg" aria-busy="true"></div>
			{:else if bdState === 'error'}
				<p class="text-bad p-4 text-[13px]" role="alert">Could not read the time breakdown.</p>
			{:else}
				<p class="text-muted-foreground px-4 py-6 text-center">{job.state === 'queued' ? 'The breakdown appears once the job has started.' : 'This job has no time breakdown.'}</p>
			{/if}
		</Panel>

		<Panel title="Result" tab="result">
			{#snippet actions()}
				{#if blobUrl}<a href={blobUrl} download="job-{job.id}-result.txt" class="{btnGhost} h-7 px-2 text-xs"><Download class="size-3.5" aria-hidden="true" />Download</a>{/if}
			{/snippet}
			{#if job.state === 'done'}
				<div class="mb-3 flex items-end gap-2">
					<div class="w-52"><Listbox label="Show result as" id="view-reducer" options={REDUCER_OPTIONS} bind:value={redId} /></div>
					{#if redId === 'top'}<label class="block w-20"><span class="mb-1 block text-xs font-medium">N</span><input class={field} type="number" min="1" step="1" inputmode="numeric" bind:value={redN} aria-invalid={!!reducer.error} /></label>{/if}
				</div>
				{#if reducer.error}
					<p class="text-bad text-[13px]" role="alert">{reducer.error}</p>
				{:else if resultError}
					<EmptyState icon={CircleAlert} title="Could not read the result" body={resultError} />
				{:else if result === null}
					<div class="bg-raised h-24 animate-pulse rounded-lg" aria-busy="true"></div>
				{:else if result.trim() === ''}
					<p class="text-muted-foreground px-4 py-6 text-center">The job finished with an empty result: nothing matched.</p>
				{:else}
					<pre class="font-mono text-xs break-all whitespace-pre-wrap" aria-label="Job result">{result.slice(0, SHOW)}</pre>
					{#if result.length > SHOW}<p class="text-muted-foreground mt-2 text-xs">Showing the first {count(SHOW)} characters. Download for all {count(result.length)}.</p>{/if}
				{/if}
			{:else}
				<p class="text-muted-foreground px-4 py-6 text-center">
					{job.state === 'queued' || job.state === 'running' ? 'The result appears here when the job is done.' : `No result: the job was ${job.state}.`}
				</p>
			{/if}
		</Panel>
		{#if hasFiles}
			<Panel title="Output files" tab="files" count={files.length || job.files}>
				{#if filesError}
					<p class="text-bad text-[13px]" role="alert">{filesError}</p>
				{:else if !files.length}
					<div class="bg-raised h-16 animate-pulse rounded-lg" aria-busy="true"></div>
				{:else}
					<ul class="divide-line border-line divide-y rounded-lg border">
						{#each files as f (f.chunk[0] + f.name)}
							<li class="flex items-center gap-2 px-3 py-2">
								<FileDown class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />
								<div class="min-w-0 flex-1">
									<p class="truncate font-medium" title={f.name}>{f.name}</p>
									<p class="num text-muted-foreground text-xs">chunk {count(f.chunk[0])} to {count(f.chunk[1] - 1)} · {fileSize(f.size)}</p>
								</div>
								<a href="/api/v1/blobs/{f.sha}" download={f.name} class="{btnGhost} h-8 px-2.5" aria-label="Download {f.name}"><Download class="size-4" aria-hidden="true" />Download</a>
							</li>
						{/each}
					</ul>
				{/if}
			</Panel>
		{/if}
	{/if}
</Page>

{#if job}
	<ConfirmDialog
		bind:open={confirmOpen}
		title="Cancel job {job.id}?"
		body="Work already done is kept, but the rest will not run. This cannot be undone."
		confirm="Cancel job"
		busy={cancelling}
		onconfirm={cancel}
	/>
{/if}
