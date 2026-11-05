<script lang="ts">
	import { page } from '$app/state';
	import { ArrowLeft, CircleAlert, Clipboard, Download, FileQuestion, Printer } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { ExperimentRow } from '#lib/api/schema.ts';
	import { download, OKABE } from '#lib/chart.ts';
	import { analyse, caption, summaryCsv } from '#lib/experiment.ts';
	import ChartLine, { type ChartSeries } from '#lib/components/ChartLine.svelte';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import ResearchPrint from '#lib/components/ResearchPrint.svelte';
	import { datetime } from '#lib/format.ts';
	import { btnGhost } from '#lib/ui.ts';

	const id = $derived(Number(page.params.id));
	let data = $state<{ experiment: ExperimentRow; raw_csv: string; runs_jsonl: string; sim: unknown } | null>(null);
	let error = $state<{ message: string; status: number } | null>(null);
	let loading = $state(true);
	let copied = $state(false);

	$effect(() => {
		const eid = id;
		loading = true;
		data = error = null;
		if (!Number.isInteger(eid)) return void (loading = false);
		api.experiment(eid)
			.then((r) => (data = r))
			.catch((e) => (error = e instanceof ApiError ? { message: e.message, status: e.status } : { message: 'Could not load the experiment.', status: 0 }))
			.finally(() => (loading = false));
	});

	const a = $derived(data ? analyse(data.raw_csv, data.runs_jsonl, data.sim) : null);
	const color = (i: number) => OKABE[i % OKABE.length]!;
	type Key = 'S' | 'E' | 'KF';
	const lo = { S: 'Slo', E: 'Elo', KF: 'KFlo' } as const, hi = { S: 'Shi', E: 'Ehi', KF: 'KFhi' } as const;
	const k = (m: Key, p: Record<string, number>, f = 1) => ({ y: p[m]! * f, lo: p[lo[m]]! * f, hi: p[hi[m]]! * f });
	function build(m: Key, f = 1): ChartSeries[] {
		if (!a) return [];
		const out: ChartSeries[] = [];
		a.measured.forEach((s, i) => {
			const tag = s.policy ? `${s.policy} ` : '';
			const pts = s.pts.filter((p) => m !== 'KF' || p.pcs > 1);
			out.push({ name: `${tag}measured`, color: color(i), dots: true, pts: pts.map((p) => ({ x: p.pcs, ...k(m, p as unknown as Record<string, number>, f) })) });
			const sim = a.sim.find((x) => x.policy === s.policy) ?? (a.sim.length === 1 ? a.sim[0] : undefined);
			if (sim) out.push({ name: `${tag}simulated`, color: color(i), dash: '5 3', pts: sim.pts.filter((p) => m !== 'KF' || p.pcs > 1).map((p) => ({ x: p.pcs, y: (p as unknown as Record<string, number>)[m]! * f })) });
		});
		return out;
	}
	const ideal = $derived<ChartSeries[]>(a ? [{ name: 'ideal', color: 'var(--muted-foreground)', dash: '1 3', pts: [...new Set(a.measured.flatMap((s) => s.pts.map((p) => p.pcs)))].sort((x, y) => x - y).map((p) => ({ x: p, y: p })) }] : []);
	const speed = $derived([...ideal, ...build('S')]);
	const eff = $derived(build('E', 100));
	const kf = $derived(build('KF'));
	const cap = $derived(data && a ? caption(data.experiment.name, data.experiment.repeats, a, data.experiment.virtual) : '');

	const RUN_COLS: Col[] = [
		{ key: 'config', label: 'Config', w: 'minmax(110px,1.4fr)' },
		{ key: 'repeat', label: 'Rep', w: '48px', right: true },
		{ key: 'job', label: 'Job', w: '64px', right: true },
		{ key: 'seconds', label: 'Time', w: '80px', right: true },
		{ key: 'pcs', label: 'PCs', w: '52px', right: true },
		{ key: 'commit', label: 'Commit', w: '80px' }
	];
	async function copyCaption() {
		try {
			await navigator.clipboard.writeText(cap);
			copied = true;
			setTimeout(() => (copied = false), 2000);
		} catch {
			// clipboard blocked: the text is selectable in the box
		}
	}
	const facts = $derived(a && data ? [
		['Runs recorded', `${a.prov.runs} (${data.experiment.configs} configs × ${data.experiment.repeats})`],
		['Commit', a.prov.commits.join(', ') ? a.prov.commits.join(', ') + (a.prov.dirty ? ` · ${a.prov.dirty} runs with uncommitted changes` : '') : '—'],
		['Head', a.prov.heads.join(', ') || '—'],
		['Workload', a.prov.kinds.join(', ') || '—'],
		['PC models', a.prov.pcClasses.join(', ') || '—'],
		['Window', a.prov.firstMs ? `${datetime(a.prov.firstMs)} to ${datetime(a.prov.lastMs)}` : '—'],
		['Fleet', data.experiment.virtual ? 'virtual PCs' : 'real PCs'],
		['Uploaded', `${datetime(data.experiment.created_ms)} by ${data.experiment.submitter}`]
	] : []);
</script>

<Page
	title={data ? data.experiment.name : 'Experiment'}
	subtitle={data ? `${data.experiment.configs} configs, ${data.experiment.repeats} repeats, up to ${data.experiment.pcs_max} PCs` : undefined}
	tabs={[{ id: 'charts', label: 'Charts' }, { id: 'runs', label: 'Provenance' }, { id: 'export', label: 'Export' }]}
	class="sm:grid-cols-[minmax(0,2fr)_minmax(0,1fr)] sm:grid-rows-[minmax(0,5fr)_minmax(0,3fr)]"
>
	{#snippet actions()}<a href="/experiments" class={btnGhost}><ArrowLeft class="size-4" aria-hidden="true" />All experiments</a>{/snippet}

	{#if loading}
		<Panel title="Charts" tab="charts" class="sm:col-span-2 sm:row-span-2"><div class="bg-raised h-40 animate-pulse rounded-lg" aria-busy="true"></div></Panel>
	{:else if !data || !a}
		<Panel title="Experiment" tab="charts" class="sm:col-span-2 sm:row-span-2">
			{#if error && error.status !== 404}
				<EmptyState icon={CircleAlert} title="Could not load this experiment" body={error.message} />
			{:else}
				<EmptyState icon={FileQuestion} title="No such experiment" body="Experiment {page.params.id} does not exist.">
					{#snippet action()}<a href="/experiments" class={btnGhost}>See all experiments</a>{/snippet}
				</EmptyState>
			{/if}
		</Panel>
	{:else}
		<Panel title="Scaling" tab="charts" scroll={false} class="sm:col-span-2">
			{#if a.measured.length === 0}
				<EmptyState icon={CircleAlert} title="No usable rows" body="raw.csv has no row with both pcs and hashes_per_s, so there is nothing to plot." />
			{:else}
				<ResearchPrint class="flex min-h-0 flex-1 flex-col">
					<p class="mb-2 hidden print:block"><span class="text-[15px] font-semibold">{data.experiment.name}</span></p>
					<div class="grid min-h-0 flex-1 gap-4 overflow-y-auto max-lg:grid-cols-1 lg:grid-cols-3 lg:grid-rows-[minmax(0,1fr)] print:grid-cols-1 print:gap-6 [&>*]:max-lg:min-h-60">
						<ChartLine title="Speed-up" xLabel="PCs" yLabel="speed-up (×)" xlog series={speed} file="{data.experiment.name}-speedup" yFmt={(v) => String(+v.toFixed(1))} />
						<ChartLine title="Efficiency" xLabel="PCs" yLabel="efficiency (%)" xlog series={eff} yMax={110} file="{data.experiment.name}-efficiency" yFmt={(v) => String(Math.round(v))} />
						<ChartLine title="Karp-Flatt serial fraction" xLabel="PCs" yLabel="serial fraction e" xlog series={kf} file="{data.experiment.name}-karp-flatt" yFmt={(v) => String(+v.toFixed(4))} />
					</div>
					<p class="hidden pt-3 text-[11px] print:block">{cap}</p>
				</ResearchPrint>
			{/if}
		</Panel>

		<Panel title="Export" tab="export" class="sm:col-start-2 sm:row-start-2">
			<div class="flex flex-wrap gap-2">
				<button type="button" class={btnGhost} onclick={() => download(`${data!.experiment.name}-raw.csv`, data!.raw_csv)}><Download class="size-4" aria-hidden="true" />raw.csv</button>
				<button type="button" class={btnGhost} onclick={() => download(`${data!.experiment.name}-summary.csv`, summaryCsv(a))}><Download class="size-4" aria-hidden="true" />Summary CSV</button>
				<button type="button" class={btnGhost} onclick={() => window.print()}><Printer class="size-4" aria-hidden="true" />PDF figure</button>
			</div>
			<p class="text-muted-foreground mt-1 text-xs">PDF figure opens the print dialog: choose Save as PDF.</p>
			<label class="mt-3 block"><span class="mb-1 flex items-center text-xs font-medium">Caption
				<button type="button" class="text-primary ml-auto inline-flex items-center gap-1 font-medium hover:underline" onclick={copyCaption}><Clipboard class="size-3.5" aria-hidden="true" />{copied ? 'Copied' : 'Copy'}</button></span>
				<textarea readonly rows="7" class="bg-surface border-line w-full resize-none rounded-lg border px-3 py-2 text-[13px]" value={cap}></textarea>
			</label>
		</Panel>

		<Panel title="Provenance" tab="runs" scroll={false} pad={false} class="sm:row-start-2">
			<dl class="border-line grid shrink-0 grid-cols-[auto_1fr] gap-x-4 gap-y-0.5 border-b px-3 py-2 text-[13px] max-sm:hidden lg:grid-cols-[auto_1fr_auto_1fr]">
				{#each facts as [kk, v] (kk)}<dt class="text-muted-foreground">{kk}</dt><dd class="num min-w-0 truncate" title={v}>{v}</dd>{/each}
			</dl>
			<dl class="border-line shrink-0 border-b px-3 py-2 text-[13px] sm:hidden">
				{#each facts.slice(0, 4) as [kk, v] (kk)}<div class="flex gap-2"><dt class="text-muted-foreground w-24 shrink-0">{kk}</dt><dd class="num min-w-0 truncate">{v}</dd></div>{/each}
			</dl>
			<DataTable label="Runs" rows={a.runs} cols={RUN_COLS} rowKey={(r) => `${r.config}|${r.repeat}|${r.job}`} rowH={32} minW={440}>
				{#snippet cell(r, c)}
					{#if c.key === 'config'}<span class="block truncate">{r.config}</span>
					{:else if c.key === 'job'}<a class="text-primary num hover:underline" href="/jobs/{r.job}">#{r.job}</a>
					{:else if c.key === 'seconds'}<span class="num">{Number(r.seconds).toFixed(2)} s</span>
					{:else if c.key === 'pcs'}<span class="num">{Array.isArray(r.pcs) ? r.pcs.length : '—'}</span>
					{:else if c.key === 'commit'}<span class="text-muted-foreground font-mono text-xs">{r.commit || '—'}</span>
					{:else}<span class="num">{r[c.key]}</span>{/if}
				{/snippet}
				{#snippet empty()}<p class="text-muted-foreground p-4 text-center text-[13px]">runs.jsonl was empty, so there is no provenance to show.</p>{/snippet}
			</DataTable>
		</Panel>
	{/if}
</Page>
