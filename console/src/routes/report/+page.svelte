<script lang="ts">
	import { live } from '#lib/live.svelte.ts';
	import { CalendarOff, ChevronLeft, ChevronRight, CircleAlert, Printer } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { DayReport } from '#lib/api/schema.ts';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import ResearchPrint from '#lib/components/ResearchPrint.svelte';
	import { count, rate } from '#lib/format.ts';
	import { dayOf, shiftDay, validDay } from '#lib/report.ts';
	import { params, setQuery } from '#lib/urlstate.svelte.ts';
	import { btnGhost, field } from '#lib/ui.ts';

	const today = dayOf(new Date());
	const day = $derived(validDay(params().get('day')) ? params().get('day')! : today);
	let r = $state<DayReport | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	const go = (d: string) => setQuery('day', d === today ? null : d);
	$effect(() => {
		const d = day;
		loading = true;
		error = null;
		api.report(d)
			.then((x) => (r = x.report))
			.catch((e) => ((r = null), (error = e instanceof ApiError ? e.message : 'Could not load the report.')))
			.finally(() => (loading = false));
	});

	// today's numbers follow the stream: a finished job or a new event refetches, at most every 30 s
	let reportAt = 0;
	$effect(() => {
		const d = day;
		void live.events[0]?.id;
		void Object.values(live.jobs).filter((j) => j.state === 'done' || j.state === 'failed').length;
		if (d !== today || !r) return;
		const t = setTimeout(() => {
			reportAt = Date.now();
			api.report(d).then((x) => (r = x.report)).catch(() => {});
		}, Math.max(0, 30_000 - (Date.now() - reportAt)));
		return () => clearTimeout(t);
	});
	const when = $derived(new Date(day + 'T12:00:00').toLocaleDateString('en-GB', { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' }));
	const quiet = $derived(!!r && r.work_done.numbers === 0 && r.work_done.jobs_done === 0 && r.pcs.max_online === 0);
	const tiles = $derived(r ? [
		['Peak throughput', rate(r.throughput_peak), ''],
		['Mean throughput', rate(r.throughput_mean), ''],
		['Numbers processed', count(r.work_done.numbers), ''],
		['Items processed', count(r.work_done.items), ''],
		['Jobs finished', count(r.work_done.jobs_done), ''],
		['Jobs failed', count(r.work_done.jobs_failed), ''],
		['PCs online (peak)', count(r.pcs.max_online), `mean ${r.pcs.mean_online.toFixed(1)} of ${r.pcs.seen} seen`],
		['Energy', r.energy_kwh == null ? '—' : r.energy_kwh.toFixed(1), r.energy_kwh == null ? 'no meter data' : 'kWh'],
		['Busy share', (r.busy_share * 100).toFixed(0), '% of online PC time'],
		['PCs lost', count(r.failures.pcs_lost), `${r.failures.recovered_chunks} chunks recovered`],
		['Head failovers', count(r.failures.head_failovers), ''],
		['Alerts', count(r.alerts), 'opened that day']
	] : []);
	const topMax = $derived(Math.max(1e-9, ...(r?.top_jobs.map((j) => j.pc_hours) ?? [0])));
	const jobsTotal = $derived(r ? r.work_done.jobs_done + r.work_done.jobs_failed : 0);
</script>

<Page title="Run report" subtitle="One page per day: throughput, PCs, failures recovered, energy. Print it for the press or a paper.">
	{#snippet actions()}
		<div class="flex items-center gap-1.5">
			<button type="button" class="{btnGhost} px-2" aria-label="Previous day" onclick={() => go(shiftDay(day, -1))}><ChevronLeft class="size-4" aria-hidden="true" /></button>
			<label><span class="sr-only">Day</span><input type="date" class="{field} h-9 w-40" value={day} max={today} onchange={(e) => validDay(e.currentTarget.value) && go(e.currentTarget.value)} /></label>
			<button type="button" class="{btnGhost} px-2" aria-label="Next day" disabled={day >= today} onclick={() => go(shiftDay(day, 1))}><ChevronRight class="size-4" aria-hidden="true" /></button>
			<button type="button" class={btnGhost} onclick={() => window.print()} disabled={!r}><Printer class="size-4" aria-hidden="true" /><span class="max-sm:sr-only">Print</span></button>
		</div>
	{/snippet}

	<Panel title="Report" label="Day report">
		{#if loading && !r}
			<div class="bg-raised h-40 animate-pulse rounded-lg" aria-busy="true"></div>
		{:else if error}
			<EmptyState icon={CircleAlert} title="Could not load the report" body={error}>
				{#snippet action()}<button type="button" class={btnGhost} onclick={() => go(day)}>Try again</button>{/snippet}
			</EmptyState>
		{:else if r}
			<ResearchPrint>
				<h2 class="text-[15px] font-semibold">Cluster Kit run report · {when}</h2>
				{#if quiet}
					<div class="mt-3"><EmptyState icon={CalendarOff} title="Nothing ran on this day" body="No PC was online and no job finished. The head keeps per-minute samples for 48 hours, so older days may be empty." /></div>
				{:else}
					<dl class="mt-3 grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-[var(--line)] bg-[var(--line)] sm:grid-cols-3 lg:grid-cols-4 print:grid-cols-4">
						{#each tiles as [k, v, note] (k)}
							<div class="bg-surface min-w-0 px-3 py-2">
								<dt class="text-muted-foreground text-xs">{k}</dt>
								<dd class="num mt-0.5 text-[17px] leading-tight font-semibold break-words">{v}</dd>
								<dd class="text-muted-foreground num min-h-4 text-[11px]">{note}</dd>
							</div>
						{/each}
					</dl>
					<div class="mt-4 grid gap-4 md:grid-cols-2 print:grid-cols-2">
						<section aria-label="Fleet use">
							<h3 class="mb-2 text-[13px] font-semibold">Fleet use</h3>
							{#each [['Busy share of online time', r.busy_share, `${(r.busy_share * 100).toFixed(0)} %`], ['Mean PCs online of PCs seen', r.pcs.seen ? r.pcs.mean_online / r.pcs.seen : 0, `${r.pcs.mean_online.toFixed(1)} of ${r.pcs.seen}`], ['Jobs that finished', jobsTotal ? r.work_done.jobs_done / jobsTotal : 0, `${r.work_done.jobs_done} of ${jobsTotal}`]] as [name, v, txt] (name)}
								<div class="mb-2.5">
									<div class="mb-1 flex justify-between text-xs"><span class="text-muted-foreground">{name}</span><span class="num font-medium">{txt}</span></div>
									<div class="bg-raised h-2.5 overflow-hidden rounded-sm" role="img" aria-label="{name}: {txt}"><div class="bg-primary h-full" style="width:{Math.min(100, (v as number) * 100)}%"></div></div>
								</div>
							{/each}
						</section>
						<section aria-label="Top jobs">
							<h3 class="mb-2 text-[13px] font-semibold">Top jobs by PC-hours</h3>
							{#each r.top_jobs as j (j.id)}
								<div class="mb-1.5 flex items-center gap-2">
									<a href="/jobs/{j.id}" class="w-40 shrink-0 truncate text-[13px] hover:underline max-sm:w-28" title={j.name}><span class="num text-muted-foreground">#{j.id}</span> {j.name}</a>
									<div class="bg-raised h-2.5 min-w-0 flex-1 overflow-hidden rounded-sm"><div class="bg-primary h-full" style="width:{(j.pc_hours / topMax) * 100}%"></div></div>
									<span class="num w-16 shrink-0 text-right text-xs">{j.pc_hours.toFixed(1)} h</span>
								</div>
							{:else}
								<p class="text-muted-foreground text-[13px]">No job used PC time on this day.</p>
							{/each}
						</section>
					</div>
				{/if}
			</ResearchPrint>
		{/if}
	</Panel>
</Page>
