<script lang="ts">
	import { CircleAlert, Crown, Eye, UserPlus } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Candidate } from '#lib/api/schema.ts';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { Listing } from '#lib/listing.svelte.ts';
	import type { Spec } from '#lib/listing.ts';
	import { vp } from '#lib/viewport.svelte.ts';

	// Who the head would pick next: score = weighted speed, memory, stability and free (weights are under All settings).
	let { tab = 'candidates' }: { tab?: string } = $props();
	let rows = $state<Candidate[] | null>(null);
	let failed = $state('');
	const roleMeta = { head: { icon: Crown, label: 'Head', cls: 'text-primary bg-primary/10' }, standby: { icon: Eye, label: 'Standby', cls: 'text-ok bg-ok/10' }, candidate: { icon: UserPlus, label: 'Candidate', cls: 'text-muted-foreground bg-raised' } } as const;
	const parts = ['speed', 'memory', 'stability', 'free'] as const;
	const pct = (x: number) => Math.round(x * 100);

	const spec: Spec<Candidate> = {
		facets: [
			{ key: 'role', label: 'Role', get: (c) => c.role, order: ['head', 'standby', 'candidate'] },
			{ key: 'eligible', label: 'Can be a standby', get: (c) => (c.eligible || c.role !== 'candidate' ? 'yes' : 'no'), single: true, order: ['yes', 'no'], text: (v) => (v === 'yes' ? 'Eligible' : 'Not eligible') }
		],
		text: (c) => [c.host, c.lab, c.role, c.why],
		sorters: { host: (c) => c.host, score: (c) => c.score, speed: (c) => c.parts.speed, memory: (c) => c.parts.memory, stability: (c) => c.parts.stability, free: (c) => c.parts.free, role: (c) => c.role, why: (c) => c.why },
		base: (a, b) => b.score - a.score
	};
	const l = new Listing<Candidate>(spec, 'c.');
	const res = $derived(l.run(rows ?? []));
	const COLS: (Col & { min?: number })[] = [
		{ key: 'host', label: 'PC', w: 'minmax(120px,1.2fr)', sort: true },
		{ key: 'score', label: 'Score', w: '150px', sort: true },
		...parts.map((p): Col & { min?: number } => ({ key: p, label: p[0]!.toUpperCase() + p.slice(1), w: '72px', right: true, sort: true, min: 1700 })),
		{ key: 'role', label: 'Role', w: '112px', sort: true },
		{ key: 'why', label: 'Why not', w: 'minmax(120px,1fr)', sort: true, min: 640 }
	];
	const cols = $derived<Col[]>(COLS.filter((c) => vp.w >= (c.min ?? 0)));

	// refresh when the head or the standby list changes, and now and then (stability moves)
	const stamp = $derived(`${live.ha?.epoch}:${live.ha?.standbys.map((s) => s.node_id).join(',')}`);
	$effect(() => {
		void stamp;
		void live.rev.candidates;
		let live_ = true;
		const load = () => api.candidates().then((r) => live_ && ((rows = r.candidates), (failed = ''))).catch((e) => live_ && (failed = e instanceof ApiError ? e.message : 'The head did not answer.'));
		void load();
		const t = setInterval(load, 60_000);
		return () => { live_ = false; clearInterval(t); };
	});
</script>

<Panel title="Who would be picked next" {tab} label="Head candidates" count={rows?.length} scroll={false} pad={false}>
	{#snippet toolbar()}
		<ListToolbar {l} rows={rows ?? []} shown={res.rows.length} total={rows?.length ?? 0} placeholder="Search PC, lab, reason" hotkey={false} />
	{/snippet}
	{#if rows === null && !failed}
		<div class="bg-raised m-3 h-40 animate-pulse rounded-lg" aria-busy="true"></div>
	{:else if failed && rows === null}
		<div class="p-3"><EmptyState icon={CircleAlert} title="Could not load the ranking" body="{failed} The standby list still works." /></div>
	{:else}
		<DataTable label="PCs ranked by head score" rows={res.rows} {cols} rowKey={(c) => c.node_id} sort={l.state.sort} dir={l.state.dir} onsort={(c) => l.sortBy(c)} rowClass={(c) => (c.eligible || c.role !== 'candidate' ? '' : 'text-muted-foreground')}>
			{#snippet cell(c, col)}
				{#if col.key === 'host'}
					<span class="block truncate font-medium" title={c.host}><Hl text={c.host} q={l.state.q} /></span>
					{#if c.lab}<span class="text-muted-foreground block truncate text-xs leading-none">{c.lab}</span>{/if}
				{:else if col.key === 'score'}
					<span class="flex items-center gap-2">
						<span class="bg-raised h-2 w-16 shrink-0 rounded-full" role="img" aria-label="Score {c.score} of 100" title="speed {pct(c.parts.speed)} · memory {pct(c.parts.memory)} · stability {pct(c.parts.stability)} · free {pct(c.parts.free)}"><span class="bg-primary block h-full rounded-full" style="width:{Math.max(0, Math.min(100, c.score))}%"></span></span>
						<span class="num w-9 text-right font-semibold">{Math.round(c.score)}</span>
					</span>
				{:else if col.key === 'role'}
					{@const m = roleMeta[c.role]}
					<span class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium {m.cls}"><m.icon class="size-3.5" aria-hidden="true" />{m.label}</span>
				{:else if col.key === 'why'}<span class="block truncate text-xs" title={c.why}>{c.why || (c.role === 'candidate' ? 'can be a standby' : '')}</span>
				{:else}<span class="num">{pct(c.parts[col.key as (typeof parts)[number]])}</span>
				{/if}
			{/snippet}
			{#snippet empty()}
				<div class="p-3">
					{#if (rows?.length ?? 0) === 0}
						<EmptyState icon={UserPlus} title="No PC to rank yet" body="Once PCs join, they are scored here." />
					{:else}
						<EmptyState icon={UserPlus} title="No PC matches these filters" body="Nothing fits the search and chips you chose.">
							{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
						</EmptyState>
					{/if}
				</div>
			{/snippet}
		</DataTable>
	{/if}
</Panel>
