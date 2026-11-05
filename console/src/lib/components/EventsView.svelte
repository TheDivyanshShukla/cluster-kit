<script lang="ts">
	import { CalendarClock, CircleAlert } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { EventRow } from '#lib/api/schema.ts';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { datetime } from '#lib/format.ts';
	import { mergeRows } from '#lib/jobs.ts';
	import { Listing } from '#lib/listing.svelte.ts';
	import { KIND_GROUPS, kindGroup, rangeLabel, rangesOf, RANGES, type Spec } from '#lib/listing.ts';
	import { setQuery } from '#lib/urlstate.svelte.ts';
	import { isPhone, vp } from '#lib/viewport.svelte.ts';
	import { btnGhost } from '#lib/ui.ts';

	let items = $state<EventRow[]>([]);
	let next = $state<number | null>(null);
	let loading = $state(true);
	let busy = false;
	let error = $state<string | null>(null);
	let focus = $state<number | null>(null);
	let now = $state(Date.now());
	$effect(() => {
		const t = setInterval(() => (now = Date.now()), 20_000);
		return () => clearInterval(t);
	});

	async function load(before?: number | null) {
		busy = true;
		try {
			const r = await api.events(200, before);
			items = before == null ? r.events : [...items, ...r.events];
			next = r.next;
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load events.';
		}
		loading = busy = false;
	}
	$effect(() => void load());
	const older = () => next != null && !busy && void load(next);

	const all = $derived(mergeRows(items, live.events));
	const spec: Spec<EventRow> = {
		facets: [
			{ key: 'group', label: 'Kind', get: (e) => kindGroup(e.kind), order: KIND_GROUPS },
			{ key: 'actor', label: 'Actor', get: (e) => e.actor || 'system' },
			{ key: 'range', label: 'When', get: (e) => rangesOf(e.ts_ms, now), single: true, order: RANGES.map((r) => r.key), text: rangeLabel }
		],
		text: (e) => [e.kind, e.actor, e.detail, e.id],
		sorters: { ts: (e) => e.ts_ms, kind: (e) => e.kind, actor: (e) => e.actor },
		base: (a, b) => b.id - a.id
	};
	const l = new Listing<EventRow>(spec);
	const res = $derived(l.run(all));
	const cur = $derived(all.find((e) => e.id === focus));
	const COLS: (Col & { min?: number })[] = [
		{ key: 'ts', label: 'Time', w: '170px', sort: true },
		{ key: 'kind', label: 'Kind', w: '130px', sort: true },
		{ key: 'actor', label: 'Actor', w: '90px', sort: true, min: 700 },
		{ key: 'detail', label: 'Detail', w: 'minmax(180px,1fr)' }
	];
	const cols = $derived<Col[]>(COLS.filter((c) => vp.w >= (c.min ?? 0)));
	const open = (e: EventRow) => ((focus = e.id), isPhone() && setQuery('tab', 'detail'));
</script>

<Panel title="List" tab="list" label="Events" count={all.length} scroll={false} pad={false}>
	{#snippet toolbar()}
		<ListToolbar {l} rows={all} shown={res.rows.length} total={all.length} placeholder="Search events" />
	{/snippet}
	{#if loading}
		<div class="bg-raised m-3 h-40 animate-pulse rounded-lg" aria-busy="true"></div>
	{:else if error && all.length === 0}
		<div class="p-4">
			<EmptyState icon={CircleAlert} title="Could not load events" body={error}>
				{#snippet action()}<button type="button" class={btnGhost} onclick={() => ((loading = true), load())}>Try again</button>{/snippet}
			</EmptyState>
		</div>
	{:else}
		<DataTable label="Events" rows={res.rows} {cols} rowH={36} rowKey={(e) => e.id} sort={l.state.sort} dir={l.state.dir} onsort={(c) => l.sortBy(c)} active={(e) => e.id === focus} onrow={open} onend={older} pill={!l.state.sort}>
			{#snippet cell(e, c)}
				{#if c.key === 'ts'}<span class="num text-muted-foreground whitespace-nowrap">{datetime(e.ts_ms)}</span>
				{:else if c.key === 'kind'}<span class="block truncate font-mono text-xs"><Hl text={e.kind} q={l.state.q} /></span>
				{:else if c.key === 'actor'}<span class="block truncate"><Hl text={e.actor || '—'} q={l.state.q} /></span>
				{:else}<span class="block truncate" title={e.detail}><Hl text={e.detail} q={l.state.q} /></span>
				{/if}
			{/snippet}
			{#snippet empty()}
				<div class="p-4">
					{#if all.length === 0}
						<EmptyState icon={CalendarClock} title="Nothing logged yet" body="Logins, job changes and PCs joining are recorded here." />
					{:else}
						<EmptyState icon={CalendarClock} title="No event matches these filters" body={next != null ? 'Older events are not loaded yet: scroll the list to the end, or clear the filters.' : 'Nothing fits the search and chips you chose.'}>
							{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
						</EmptyState>
					{/if}
				</div>
			{/snippet}
		</DataTable>
	{/if}
</Panel>

<Panel title="Detail" tab="detail" label="Selected event">
	{#if cur}
		<dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-[13px]">
			<dt class="text-muted-foreground">Time</dt><dd class="num">{datetime(cur.ts_ms)}</dd>
			<dt class="text-muted-foreground">Kind</dt><dd class="font-mono text-xs">{cur.kind}</dd>
			<dt class="text-muted-foreground">Actor</dt><dd>{cur.actor || 'system'}</dd>
			<dt class="text-muted-foreground">Detail</dt><dd class="break-words">{cur.detail}</dd>
			<dt class="text-muted-foreground">Id</dt><dd class="num">{cur.id}</dd>
		</dl>
	{:else}
		<p class="text-muted-foreground text-[13px]">Click an event to read it in full here.</p>
	{/if}
</Panel>
