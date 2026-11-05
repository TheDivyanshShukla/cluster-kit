<script lang="ts">
	import { live } from '#lib/live.svelte.ts';
	import { CircleAlert, FlaskConical } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { ExperimentRow } from '#lib/api/schema.ts';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { ago, datetime } from '#lib/format.ts';
	import { Listing } from '#lib/listing.svelte.ts';
	import type { Spec } from '#lib/listing.ts';
	import { vp } from '#lib/viewport.svelte.ts';
	import { btnGhost } from '#lib/ui.ts';

	let rows0 = $state<ExperimentRow[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	async function load() {
		try {
			rows0 = (await api.experiments()).experiments;
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load experiments.';
		}
		loading = false;
	}
	$effect(() => void load());
	// refetch when the head reports an experiment event (if it never does: when the window regains focus)
	let lastEv = 0;
	$effect(() => {
		const e = live.events[0];
		if (e && e.id !== lastEv) {
			if (lastEv && e.kind.startsWith('experiment.')) void load();
			lastEv = e.id;
		}
	});
	$effect(() => {
		const f = () => void load();
		addEventListener('focus', f);
		return () => removeEventListener('focus', f);
	});

	const spec: Spec<ExperimentRow> = {
		facets: [
			{ key: 'by', label: 'By', get: (e) => e.submitter },
			{ key: 'machines', label: 'PCs', get: (e) => (e.virtual ? 'Virtual' : 'Real') },
			{ key: 'commit', label: 'Commit', get: (e) => e.commit || 'unknown' }
		],
		text: (e) => [e.id, e.name, e.submitter, e.commit],
		sorters: { name: (e) => e.name, by: (e) => e.submitter, created: (e) => e.created_ms, configs: (e) => e.configs, repeats: (e) => e.repeats, pcs: (e) => e.pcs_max },
		base: (a, b) => b.created_ms - a.created_ms
	};
	const l = new Listing<ExperimentRow>(spec);
	const res = $derived(l.run(rows0));
	const COLS: (Col & { min?: number })[] = [
		{ key: 'name', label: 'Experiment', w: 'minmax(140px,2fr)', sort: true },
		{ key: 'by', label: 'By', w: '84px', sort: true, min: 700 },
		{ key: 'configs', label: 'Configs', w: '80px', right: true, sort: true, min: 560 },
		{ key: 'repeats', label: 'Repeats', w: '80px', right: true, sort: true },
		{ key: 'pcs', label: 'Max PCs', w: '86px', right: true, sort: true },
		{ key: 'commit', label: 'Commit', w: '90px', min: 1000 },
		{ key: 'created', label: 'Uploaded', w: '120px', sort: true, min: 560 }
	];
	const cols = $derived<Col[]>(COLS.filter((c) => vp.w >= (c.min ?? 0)));
</script>

<Page title="Experiments" subtitle="Scaling runs uploaded by scripts/experiment.py --upload, with their provenance and the simulator prediction.">
	<Panel title="Experiments" count={rows0.length} scroll={false} pad={false}>
		{#snippet toolbar()}
			<ListToolbar {l} rows={rows0} shown={res.rows.length} total={rows0.length} placeholder="Search experiments" />
		{/snippet}
		{#if loading}
			<div class="bg-raised m-3 h-40 animate-pulse rounded-lg" aria-busy="true"></div>
		{:else if error && rows0.length === 0}
			<div class="p-4">
				<EmptyState icon={CircleAlert} title="Could not load experiments" body={error}>
					{#snippet action()}<button type="button" class={btnGhost} onclick={() => ((loading = true), load())}>Try again</button>{/snippet}
				</EmptyState>
			</div>
		{:else}
			<DataTable label="Experiments" rows={res.rows} {cols} rowKey={(e) => e.id} sort={l.state.sort} dir={l.state.dir} onsort={(c) => l.sortBy(c)} onrow={(e) => (location.href = `/experiments/${e.id}`)}>
				{#snippet cell(e, c)}
					{#if c.key === 'name'}
						<a href="/experiments/{e.id}" class="block truncate font-medium hover:underline"><Hl text={e.name} q={l.state.q} />{#if e.virtual}<span class="bg-raised text-muted-foreground ml-2 rounded-full px-1.5 py-0.5 text-[11px] font-normal">virtual</span>{/if}</a>
					{:else if c.key === 'by'}<span class="block truncate"><Hl text={e.submitter} q={l.state.q} /></span>
					{:else if c.key === 'configs'}<span class="num">{e.configs}</span>
					{:else if c.key === 'repeats'}<span class="num">{e.repeats}</span>
					{:else if c.key === 'pcs'}<span class="num">{e.pcs_max}</span>
					{:else if c.key === 'commit'}<span class="text-muted-foreground font-mono text-xs"><Hl text={e.commit || '—'} q={l.state.q} /></span>
					{:else}<span class="text-muted-foreground whitespace-nowrap" title={datetime(e.created_ms)}>{ago(e.created_ms)}</span>{/if}
				{/snippet}
				{#snippet empty()}
					<div class="p-4">
						{#if rows0.length === 0}
							<EmptyState icon={FlaskConical} title="No experiment uploaded yet" body="Run python3 scripts/experiment.py --pcs 1,2,4,8 --repeats 7 --upload. The result appears here with its charts." />
						{:else}
							<EmptyState icon={FlaskConical} title="No experiment matches these filters" body="Nothing fits the search and chips you chose.">
								{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
							</EmptyState>
						{/if}
					</div>
				{/snippet}
			</DataTable>
		{/if}
	</Panel>
</Page>
