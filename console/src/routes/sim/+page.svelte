<script lang="ts">
	import { Lock, Play, TriangleAlert } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { OKABE } from '#lib/chart.ts';
	import ChartLine, { type ChartSeries } from '#lib/components/ChartLine.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { checkScenario, hoursOf, isDay, normalizeSim, PRESETS, SIM_MAX_HOURS, SIM_MAX_NODES, totalPcs, type Scenario, type SimView } from '#lib/sim.ts';
	import { setQuery } from '#lib/urlstate.svelte.ts';
	import { btnGhost, btnPrimary, field } from '#lib/ui.ts';

	// Simulation Lab (SIM-05, UI-09): the `ck sim` scenario as a form plus JSON, run on the head, results as stats and charts.
	const pretty = (s: Scenario) => JSON.stringify(s, null, 2);
	let preset = $state(PRESETS[0]!.id);
	let text = $state(pretty(PRESETS[0]!.scenario));
	let busy = $state(false);
	let error = $state('');
	let result = $state<SimView | null>(null);
	let took = $state(0);

	const parsed = $derived.by((): { ok: true; s: Scenario } | { ok: false; why: string } => {
		try {
			const s = JSON.parse(text);
			const why = checkScenario(s);
			return why ? { ok: false, why } : { ok: true, s };
		} catch (e) {
			return { ok: false, why: `Not valid JSON: ${(e as Error).message}` };
		}
	});
	const sc = $derived(parsed.ok ? parsed.s : null);
	const allowed = $derived(atLeast(roleNow(), 'researcher'));

	function load(id: string) {
		preset = id;
		text = pretty(PRESETS.find((p) => p.id === id)!.scenario);
		result = null;
		error = '';
	}
	// the form edits the JSON, so both always agree
	function patch(fn: (s: Scenario) => void) {
		try {
			const s = JSON.parse(text);
			fn(s);
			text = pretty(s);
		} catch {
			// invalid JSON: the form waits until the text parses again
		}
	}
	const setNum = (e: Event, fn: (s: Scenario, v: number) => void) => {
		const v = Number((e.currentTarget as HTMLInputElement).value);
		if (Number.isFinite(v) && v > 0) patch((s) => fn(s, v));
	};

	async function run() {
		if (!sc) return;
		busy = true;
		error = '';
		const t0 = performance.now();
		try {
			result = normalizeSim(await api.sim(sc));
			took = performance.now() - t0;
			setQuery('tab', 'result');
		} catch (e) {
			error = e instanceof ApiError ? (e.status === 409 ? 'Another simulation is running on the head. Try again in a moment.' : e.message) : 'The head did not answer.';
		}
		busy = false;
	}

	// charts: thin to ~240 points, time in the unit that reads best
	function pts(v: SimView, key: string, f = (y: number) => y) {
		const rows = v.rows.filter((r) => key in r);
		const step = Math.max(1, Math.ceil(rows.length / 240));
		const span = v.rows.at(-1)?.t ?? 0;
		const div = v.kind === 'day' ? 3600 : span > 7200 ? 3600 : span > 300 ? 60 : 1;
		return rows.filter((_, i) => i % step === 0).map((r) => ({ x: r.t / div, y: f(r[key]!) }));
	}
	const unit = (v: SimView) => {
		const span = v.rows.at(-1)?.t ?? 0;
		return v.kind === 'day' || span > 7200 ? 'hours' : span > 300 ? 'minutes' : 'seconds';
	};
	const charts = $derived.by((): { title: string; y: string; series: ChartSeries[]; max?: number; fmt?: (v: number) => string }[] => {
		const v = result;
		if (!v || !v.rows.length) return [];
		const S = (name: string, key: string, i: number, f?: (y: number) => number): ChartSeries => ({ name, color: OKABE[i]!, pts: pts(v, key, f) });
		const peak = Math.max(0, ...v.rows.map((r) => r.rate ?? 0));
		const [div, unitName] = peak >= 1e9 ? [1e9, 'billion items/s'] : peak >= 1e6 ? [1e6, 'million items/s'] : [1, 'items/s'];
		return v.kind === 'job'
			? [
					{ title: 'Work rate', y: unitName, series: [S('rate', 'rate', 0, (y) => y / div)] },
					{ title: 'PCs', y: 'PCs', series: [S('connected', 'connected', 2), S('busy', 'busy', 1)] },
					{ title: 'Head load', y: 'head busy (%)', series: [S('head', 'head', 5, (y) => y * 100)], max: 100, fmt: (y) => String(Math.round(y)) }
				]
			: [
					{ title: 'PCs by state', y: 'PCs', series: [S('busy', 'busy', 1), S('idle', 'idle', 2), S('asleep', 'asleep', 4), S('booting', 'booting', 5), S('class', 'class', 3)] },
					{ title: 'Power draw', y: 'kW', series: [S('draw', 'watts', 0, (y) => y / 1000)] }
				];
	});
	const archs = ['single', 'standby', 'relays', 'relays+standby'];
	const policies = ['adaptive', 'guided', 'fixed'];
</script>

<Page
	title="Simulation"
	subtitle="Try a cluster before building it: PCs, labs, failures, relays, eco sleep. Runs on the head, on its own thread."
	tabs={[{ id: 'scenario', label: 'Scenario' }, { id: 'result', label: 'Result' }]}
	class="sm:grid-cols-[minmax(20rem,26rem)_minmax(0,1fr)]"
>
	{#if !allowed}
		<Panel title="Scenario" tab="scenario" class="sm:col-span-2"><EmptyState icon={Lock} title="Researchers and above" body="Running a simulation needs the researcher role. Ask an admin if you need it." /></Panel>
	{:else}
		<Panel title="Scenario" tab="scenario" scroll={false} pad={false}>
			<div class="thin flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-3">
				<label class="block text-[13px]"><span class="mb-1 block font-medium">Start from</span>
					<select class={field} value={preset} onchange={(e) => load(e.currentTarget.value)}>{#each PRESETS as p (p.id)}<option value={p.id}>{p.label}</option>{/each}</select>
					<span class="text-muted-foreground mt-1 block text-xs">{PRESETS.find((p) => p.id === preset)?.note}</span>
				</label>
				{#if sc}
					<p class="num text-muted-foreground text-[13px]">{totalPcs(sc).toLocaleString('en')} PCs{isDay(sc) ? `, ${hoursOf(sc)} h` : ''}. Limits: {SIM_MAX_NODES.toLocaleString('en')} PCs, {SIM_MAX_HOURS} h.</p>
					<div class="grid grid-cols-2 gap-3">
						{#if isDay(sc)}
							<label class="text-[13px]"><span class="mb-1 block font-medium">Hours</span><input class="{field} num" type="number" min="1" max={SIM_MAX_HOURS} value={sc.day?.hours ?? 24} onchange={(e) => setNum(e, (s, v) => ((s.day ??= {}).hours = v))} /></label>
							<label class="flex min-h-9 items-center gap-2 self-end text-[13px]"><input type="checkbox" role="switch" class="size-4" checked={!!sc.eco?.enabled} onchange={(e) => patch((s) => ((s.eco ??= {}).enabled = e.currentTarget.checked))} />Eco sleep</label>
						{:else}
							<label class="text-[13px]"><span class="mb-1 block font-medium">Job size (items)</span><input class="{field} num" type="number" min="1" step="any" value={sc.job?.n} onchange={(e) => setNum(e, (s, v) => ((s.job ??= {}).n = v))} /></label>
							<label class="text-[13px]"><span class="mb-1 block font-medium">Split policy</span>
								<select class={field} value={sc.job?.policy ?? 'adaptive'} onchange={(e) => patch((s) => ((s.job ??= {}).policy = e.currentTarget.value))}>{#each policies as p (p)}<option>{p}</option>{/each}</select>
							</label>
							<label class="text-[13px]"><span class="mb-1 block font-medium">Head and relays</span>
								<select class={field} value={sc.architecture ?? 'single'} onchange={(e) => patch((s) => (s.architecture = e.currentTarget.value))}>{#each archs as a (a)}<option>{a}</option>{/each}</select>
							</label>
							<label class="text-[13px]"><span class="mb-1 block font-medium">Head fails at (s)</span><input class="{field} num" type="number" min="0" placeholder="never" value={sc.head_failure?.at_s ?? ''} onchange={(e) => { const v = e.currentTarget.value; patch((s) => (v === '' ? delete s.head_failure : (s.head_failure = { recovery_s: 6, ...s.head_failure, at_s: Number(v) }))); }} /></label>
						{/if}
						<label class="text-[13px]"><span class="mb-1 block font-medium">Random seed</span><input class="{field} num" type="number" min="0" value={(isDay(sc) ? sc.day?.seed : sc.seed) ?? 1} onchange={(e) => setNum(e, (s, v) => (isDay(s) ? ((s.day ??= {}).seed = v) : (s.seed = v)))} /></label>
					</div>
				{/if}
				<label class="flex min-h-40 flex-1 flex-col text-[13px]"><span class="mb-1 block font-medium">Scenario JSON <span class="text-muted-foreground font-normal">(same format as <code>ck sim</code>)</span></span>
					<textarea class="{field} min-h-40 flex-1 resize-none font-mono text-xs!" bind:value={text} spellcheck="false" autocomplete="off" aria-invalid={!parsed.ok} aria-describedby="sim-why"></textarea>
				</label>
				<p id="sim-why" class="text-bad min-h-4 text-[13px]" role="alert">{parsed.ok ? '' : parsed.why}</p>
			</div>
			{#snippet footer()}
				<div class="flex flex-wrap items-center gap-2">
					<button type="button" class={btnPrimary} disabled={busy || !parsed.ok} onclick={() => void run()}><Play class="size-4" aria-hidden="true" />{busy ? 'Running…' : 'Run simulation'}</button>
					<button type="button" class={btnGhost} onclick={() => load(preset)}>Reset</button>
				</div>
			{/snippet}
		</Panel>

		<Panel title="Result" tab="result" label="Simulation result">
			{#if error}
				<p class="text-bad flex items-start gap-2 text-[13px]" role="alert"><TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />{error}</p>
			{:else if busy}
				<div class="space-y-3" aria-busy="true"><div class="bg-raised h-16 animate-pulse rounded-lg"></div><div class="bg-raised h-48 animate-pulse rounded-lg"></div></div>
			{:else if !result}
				<p class="text-muted-foreground text-[13px]">Pick a scenario and press Run. The head simulates it without touching the real cluster; most runs take a second or two.</p>
			{:else}
				<p class="mb-3 font-medium" role="status">{result.headline} <span class="text-muted-foreground num text-xs font-normal">in {(took / 1000).toFixed(1)} s</span></p>
				<dl class="mb-4 grid grid-cols-2 gap-2 lg:grid-cols-3">
					{#each result.stats as st (st.label)}
						<div class="bg-raised min-w-0 rounded-lg px-3 py-2">
							<dt class="text-muted-foreground truncate text-xs">{st.label}</dt>
							<dd class="num truncate text-base font-semibold" title={st.value}>{st.value}</dd>
							{#if st.hint}<dd class="text-muted-foreground truncate text-xs" title={st.hint}>{st.hint}</dd>{/if}
						</div>
					{/each}
				</dl>
				<div class="grid gap-4 lg:grid-cols-2">
					{#each charts as c (c.title)}
						<div class="flex h-56 min-w-0"><ChartLine title={c.title} xLabel={unit(result)} yLabel={c.y} series={c.series} yMax={c.max} yFmt={c.fmt} file="sim-{c.title.toLowerCase().replaceAll(' ', '-')}" /></div>
					{/each}
				</div>
				{#if result.jobs.length}
					<table class="mt-4 w-full text-[13px]" aria-label="Jobs in the simulated day">
						<thead><tr class="text-muted-foreground text-left text-xs"><th class="py-1 font-normal">Job</th><th class="py-1 text-right font-normal">Waited</th><th class="py-1 text-right font-normal">Took</th><th class="py-1 text-right font-normal">Peak PCs</th></tr></thead>
						<tbody class="divide-line divide-y">
							{#each result.jobs as j (j.name)}
								<tr><td class="max-w-0 truncate py-1" title={j.name}>{j.name}</td><td class="num py-1 text-right">{j.wait_s == null ? '—' : `${Math.round(j.wait_s)} s`}</td><td class="num py-1 text-right">{j.makespan_s == null ? 'unfinished' : `${Math.round(j.makespan_s / 60)} min`}</td><td class="num py-1 text-right">{j.peak_pcs}</td></tr>
							{/each}
						</tbody>
					</table>
				{/if}
			{/if}
		</Panel>
	{/if}
</Page>
