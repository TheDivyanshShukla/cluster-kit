<script lang="ts">
	import { page } from '$app/state';
	import { ArrowLeft, CircleAlert, Clock, Download, FileQuestion, Square } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import Listbox from '#lib/components/Listbox.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import PriorityChip from '#lib/components/PriorityChip.svelte';
	import ProgressBar from '#lib/components/ProgressBar.svelte';
	import StateChip from '#lib/components/StateChip.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { count, datetime, duration, rate } from '#lib/format.ts';
	import { canCancel, elapsedMs, etaMs, jobRate, progress } from '#lib/jobs.ts';
	import { etaLeft, joinReducer, POLICIES, reducerLabel, REDUCER_OPTIONS, splitReducer } from '#lib/sched.ts';
	import { roleNow, userNow } from '#lib/session.svelte.ts';
	import { btnDanger, btnGhost, card, field } from '#lib/ui.ts';

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
			})
			.catch((e) => (error = e instanceof ApiError ? { message: e.message, status: e.status } : { message: 'Could not load the job.', status: 0 }))
			.finally(() => (fetched = true));
	});

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
					['Reducer', job.reducer ? reducerLabel(job.reducer) : 'Raw parts'],
					['Time left', job.state === 'running' ? (etaWords?.replace(' left', '') ?? duration(etaMs(job, now))) : '—']
				]
			: []
	);
</script>

<svelte:head><title>{job ? `Job ${job.id}` : 'Job'} · Cluster Kit</title></svelte:head>

<a href="/jobs" class="text-muted-foreground hover:text-foreground inline-flex items-center gap-1.5 text-[13px]"><ArrowLeft class="size-4" aria-hidden="true" />All jobs</a>

{#if !job && !fetched}
	<div class="{card} mt-4 h-56 animate-pulse" aria-busy="true"></div>
{:else if !job}
	<div class="mt-4">
		{#if error && error.status !== 404}
			<EmptyState icon={CircleAlert} title="Could not load this job" body={error.message} />
		{:else}
			<EmptyState icon={FileQuestion} title="No such job" body="Job {page.params.id} does not exist. It may have been mistyped.">
				{#snippet action()}<a href="/jobs" class={btnGhost}>See all jobs</a>{/snippet}
			</EmptyState>
		{/if}
	</div>
{:else}
	<div class="mt-3 flex flex-wrap items-start justify-between gap-3">
		<div class="min-w-0">
			<h1 class="text-xl font-semibold tracking-tight break-words"><span class="num text-muted-foreground">#{job.id}</span> {job.name || 'Untitled job'}</h1>
			<p class="mt-2 flex flex-wrap items-center gap-2"><StateChip state={job.state} /><PriorityChip priority={job.priority} /></p>
		</div>
		{#if canCancel(roleNow(), userNow(), job)}
			<button type="button" class={btnDanger} onclick={() => (confirmOpen = true)}><Square class="size-4" aria-hidden="true" />Cancel job</button>
		{/if}
	</div>

	{#if cancelError}<p class="text-bad mt-3 flex items-center gap-1.5 text-[13px]" role="alert"><CircleAlert class="size-4 shrink-0" aria-hidden="true" />{cancelError}</p>{/if}
	{#if job.state === 'failed'}
		<p class="border-bad text-bad mt-4 flex items-start gap-2 rounded-lg border px-3 py-2.5" role="alert">
			<CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" /><span class="break-words">Failed: {job.error ?? 'no reason was recorded.'}</span>
		</p>
	{/if}

	<section class="{card} mt-4 px-4 py-4" aria-label="Progress">
		<ProgressBar value={progress(job)} label="Job progress" tone={job.state === 'failed' ? 'bg-bad' : job.state === 'done' ? 'bg-ok' : 'bg-primary'} />
		<p class="num text-muted-foreground mt-2 text-xs" aria-live="polite">
			{count(job.done)} of {count(job.total)} numbers · {Math.round(progress(job) * 100)}%{job.state === 'queued' ? ' · waiting for its turn' : ''}
		</p>
		{#if etaWords}<p class="mt-1 flex items-center gap-1.5 text-[13px] font-medium" aria-live="polite"><Clock class="text-muted-foreground size-4" aria-hidden="true" />{etaWords[0].toUpperCase() + etaWords.slice(1)}</p>{/if}
	</section>

	<dl class="{card} mt-4 grid grid-cols-2 gap-px overflow-hidden sm:grid-cols-3 lg:grid-cols-4">
		{#each facts as [k, v] (k)}
			<div class="bg-surface px-4 py-3">
				<dt class="text-muted-foreground text-xs">{k}</dt>
				<dd class="num mt-0.5 break-words">{v}</dd>
			</div>
		{/each}
	</dl>

	<section class="mt-6" aria-label="Result">
		<div class="mb-2 flex flex-wrap items-center justify-between gap-2">
			<h2 class="font-medium">Result</h2>
			{#if job.state === 'done'}
				<div class="flex items-end gap-2">
					<div class="w-52"><Listbox label="Show result as" id="view-reducer" options={REDUCER_OPTIONS} bind:value={redId} /></div>
					{#if redId === 'top'}<label class="block w-20"><span class="mb-1 block text-xs font-medium">N</span><input class={field} type="number" min="1" step="1" inputmode="numeric" bind:value={redN} aria-invalid={!!reducer.error} /></label>{/if}
				</div>
			{/if}
			{#if blobUrl}
				<a href={blobUrl} download="job-{job.id}-result.txt" class={btnGhost}><Download class="size-4" aria-hidden="true" />Download</a>
			{/if}
		</div>
		{#if job.state === 'done'}
			{#if reducer.error}
				<p class="text-bad text-[13px]" role="alert">{reducer.error}</p>
			{:else if resultError}
				<EmptyState icon={CircleAlert} title="Could not read the result" body={resultError} />
			{:else if result === null}
				<div class="{card} h-24 animate-pulse" aria-busy="true"></div>
			{:else if result.trim() === ''}
				<p class="text-muted-foreground {card} px-4 py-6 text-center">The job finished with an empty result: nothing matched.</p>
			{:else}
				<!-- svelte-ignore a11y_no_noninteractive_tabindex (a scrollable box must be focusable) -->
				<pre class="{card} max-h-96 overflow-auto p-3 font-mono text-xs break-all whitespace-pre-wrap" tabindex="0" aria-label="Job result">{result.slice(0, SHOW)}</pre>
				{#if result.length > SHOW}<p class="text-muted-foreground mt-2 text-xs">Showing the first {count(SHOW)} characters. Download for all {count(result.length)}.</p>{/if}
			{/if}
		{:else}
			<p class="text-muted-foreground {card} border-dashed px-4 py-6 text-center">
				{job.state === 'queued' || job.state === 'running' ? 'The result appears here when the job is done.' : `No result: the job was ${job.state}.`}
			</p>
		{/if}
	</section>

	<ConfirmDialog
		bind:open={confirmOpen}
		title="Cancel job {job.id}?"
		body="Work already done is kept, but the rest will not run. This cannot be undone."
		confirm="Cancel job"
		busy={cancelling}
		onconfirm={cancel}
	/>
{/if}
