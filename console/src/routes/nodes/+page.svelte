<script lang="ts">
	import { CircleCheck, Server, Unplug } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import AvailChips from '#lib/components/AvailChips.svelte';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import JoinCodes from '#lib/components/JoinCodes.svelte';
	import LabelChips from '#lib/components/LabelChips.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import NodeActions from '#lib/components/NodeActions.svelte';
	import NodeStateChip from '#lib/components/NodeStateChip.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { bytes, count, pct, rate } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { Listing } from '#lib/listing.svelte.ts';
	import { naturalCompare, type Spec } from '#lib/listing.ts';
	import { toItems, type Item } from '#lib/map.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { setQuery } from '#lib/urlstate.svelte.ts';
	import { isPhone, vp } from '#lib/viewport.svelte.ts';
	import { btnPrimary } from '#lib/ui.ts';

	// "lost" = should be online but is not (an asleep PC is not lost)
	const status = (i: Item) => (i.node.state !== 'active' ? i.node.state : !i.node.online && i.node.power === 'awake' ? 'lost' : 'active');
	const SEV = ['lost', 'pending', 'maintenance', 'draining', 'cordoned', 'retired', 'active'];
	const labName = (id: number | null) => (id == null ? 'Unplaced' : (live.labs.find((l) => l.id === id)?.name ?? `Lab ${id}`));
	const use = (i: Item) => (!i.node.online ? 'offline' : i.w?.busy ? 'working' : 'idle');
	const byDefault = (a: Item, b: Item) =>
		SEV.indexOf(status(a)) - SEV.indexOf(status(b)) || naturalCompare(labName(a.node.lab_id), labName(b.node.lab_id)) || naturalCompare(a.node.host, b.node.host);
	const spec: Spec<Item> = {
		facets: [
			{ key: 'state', label: 'State', get: status, order: SEV },
			{ key: 'lab', label: 'Lab', get: (i) => labName(i.node.lab_id) },
			{ key: 'power', label: 'Power', get: (i) => i.node.power, order: ['awake', 'waking', 'asleep'] },
			{ key: 'use', label: 'In use', get: use, order: ['working', 'idle', 'offline'] }
		],
		text: (i) => [i.node.host, i.node.addr, i.node.node_id, status(i), labName(i.node.lab_id), i.node.inventory?.cpu_model, ...Object.entries(i.node.labels).map(([k, v]) => `${k}=${v}`), ...i.node.pools],
		sorters: {
			host: (i) => i.node.host,
			state: (i) => SEV.indexOf(status(i)),
			lab: (i) => labName(i.node.lab_id),
			threads: (i) => i.w?.threads ?? i.node.inventory?.cores_logical,
			ram: (i) => i.node.inventory?.ram_bytes ?? i.w?.mem_total,
			cpu: (i) => i.w?.cpu,
			rate: (i) => i.w?.rate
		},
		base: byDefault
	};
	const l = new Listing<Item>(spec);

	let sel = $state(typeof location === 'undefined' ? '' : (new URLSearchParams(location.search).get('sel') ?? ''));
	let matched = $state<string[] | null>(null);
	let selected = $state<string[]>([]);
	let focus = $state<string | null>(null);
	let err = $state('');
	let busy = $state(false);
	const canAct = $derived(atLeast(roleNow(), 'operator'));
	const all = $derived(toItems(live.nodes, live.cluster?.workers ?? []));
	const pending = $derived(all.filter((i) => i.node.state === 'pending'));
	const res = $derived(l.run(all));
	const rows = $derived(matched ? res.rows.filter((i) => matched!.includes(i.node.node_id)) : res.rows);
	const allOn = $derived(rows.length > 0 && rows.every((r) => selected.includes(r.node.node_id)));
	const cur = $derived(all.find((i) => i.node.node_id === focus));

	const COLS: (Col & { min?: number })[] = [
		{ key: 'host', label: 'PC', w: 'minmax(96px,1.6fr)', sort: true },
		{ key: 'state', label: 'State', w: 'minmax(120px,150px)', sort: true },
		{ key: 'lab', label: 'Lab', w: 'minmax(80px,0.8fr)', sort: true, min: 700 },
		{ key: 'threads', label: 'Threads', w: '96px', right: true, sort: true, min: 900 },
		{ key: 'ram', label: 'RAM', w: '92px', right: true, sort: true, min: 900 },
		{ key: 'cpu', label: 'CPU', w: '88px', right: true, sort: true },
		{ key: 'rate', label: 'Rate', w: '104px', right: true, sort: true, min: 1100 }
	];
	const cols = $derived<Col[]>([...(canAct ? [{ key: 'sel', label: '', w: '36px' }] : []), ...COLS.filter((c) => vp.w >= (c.min ?? 0))]);

	function open(i: Item) {
		focus = i.node.node_id;
		if (isPhone()) setQuery('tab', 'details');
	}
	async function unlabel(id: string, key: string) {
		err = '';
		try {
			live.nodes[id] = (await api.setLabels(id, { [key]: null })).node;
		} catch (e) {
			err = e instanceof ApiError ? e.message : 'Could not remove the label.';
		}
	}
	async function approve(ids: string[]) {
		busy = true;
		err = '';
		try {
			for (const n of (await api.setNodeState(ids, 'active')).nodes) live.nodes[n.node_id] = n;
			selected = selected.filter((s) => !ids.includes(s));
		} catch (e) {
			err = e instanceof ApiError ? e.message : 'Could not approve.';
		}
		busy = false;
	}
</script>

<Page
	title="Nodes"
	subtitle="Every PC registered with this head."
	tabs={[{ id: 'list', label: 'List', badge: res.rows.length }, { id: 'details', label: 'Details', badge: selected.length || undefined }]}
	class="sm:grid-cols-[minmax(0,1fr)_17rem] lg:grid-cols-[minmax(0,1fr)_23rem]"
>
	{#snippet actions()}
		{#if pending.length && canAct}
			<button type="button" class={btnPrimary} disabled={busy} onclick={() => approve(pending.map((p) => p.node.node_id))}>
				<CircleCheck class="size-4" aria-hidden="true" />Approve {pending.length} waiting
			</button>
		{:else if pending.length}
			<span class="text-warn text-[13px]" role="status">{pending.length} waiting for approval</span>
		{/if}
		{#if canAct}<JoinCodes />{/if}
	{/snippet}

	<Panel title="List" tab="list" label="PCs" count={all.length} scroll={false} pad={false}>
		{#snippet toolbar()}
			<ListToolbar {l} rows={all} shown={rows.length} total={all.length} placeholder="Search PCs" />
		{/snippet}
		{#if !live.cluster}
			<div class="bg-raised m-3 h-40 animate-pulse rounded-lg" aria-busy="true"></div>
		{:else}
			<DataTable
				label="PCs"
				{rows}
				{cols}
				rowH={44}
				minW={0}
				rowKey={(r) => r.node.node_id}
				sort={l.state.sort}
				dir={l.state.dir}
				onsort={(c) => l.sortBy(c)}
				active={(r) => r.node.node_id === focus}
				onrow={open}
				rowClass={(r) => (r.node.state === 'pending' ? 'bg-warn/10' : r.node.online || r.node.power !== 'awake' ? '' : 'text-muted-foreground')}
			>
				{#snippet cell(r, c)}
					{@const n = r.node}
					{#if c.key === 'sel'}
						<input type="checkbox" checked={selected.includes(n.node_id)} aria-label="Select {n.host}" onchange={(e) => (selected = e.currentTarget.checked ? [...selected, n.node_id] : selected.filter((s) => s !== n.node_id))} />
					{:else if c.key === 'host'}
						<p class="truncate font-medium" title={n.host}><Hl text={n.host} q={l.state.q} /></p>
						<p class="text-muted-foreground truncate font-mono text-[11px]">{n.addr}{r.w?.virtual ? ' · virtual' : ''}</p>
					{:else if c.key === 'state'}
						<NodeStateChip state={n.state} online={n.online} power={n.power} id={n.node_id} />
					{:else if c.key === 'lab'}
						<span class="block truncate" title={n.lab_id != null && n.x != null ? `${labName(n.lab_id)} · ${n.x + 1},${(n.y ?? 0) + 1}` : ''}>{labName(n.lab_id)}</span>
					{:else if c.key === 'threads'}<span class="num">{count(r.w?.threads ?? n.inventory?.cores_logical)}</span>
					{:else if c.key === 'ram'}<span class="num">{bytes(n.inventory?.ram_bytes ?? r.w?.mem_total)}</span>
					{:else if c.key === 'cpu'}<span class="num">{r.w ? pct(r.w.cpu) : '—'}</span>
					{:else if c.key === 'rate'}<span class="num">{rate(r.w?.rate)}</span>
					{/if}
				{/snippet}
				{#snippet empty()}
					<div class="p-4">
						{#if all.length === 0}
							<EmptyState icon={Unplug} title="No PC has joined yet" body={canAct ? 'Create a join code, then run the command it shows on a lab PC.' : 'Start an agent on any PC on this network: ck agent.'} />
						{:else}
							<EmptyState icon={Server} title="No PC matches these filters" body="Nothing fits the search and chips you chose.">
								{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
							</EmptyState>
						{/if}
					</div>
				{/snippet}
			</DataTable>
		{/if}
	</Panel>

	<Panel title="Details" tab="details" count={selected.length ? `${selected.length} selected` : undefined}>
		{#snippet actions()}
			{#if canAct && rows.length}
				<label class="text-muted-foreground flex items-center gap-1.5 text-xs">
					<input type="checkbox" checked={allOn} onchange={() => (selected = allOn ? [] : rows.map((r) => r.node.node_id))} />All shown
				</label>
			{/if}
		{/snippet}
		<div class="space-y-4">
			{#if err}<p class="text-bad text-[13px]" role="alert">{err}</p>{/if}
			{#if selected.length}
				<NodeActions ids={selected} ondone={() => (selected = [])} />
			{/if}
			{#if cur}
				{@const n = cur.node}
				<section aria-label="Selected PC" class="space-y-3 text-[13px]">
					<div>
						<p class="text-[15px] font-semibold">{n.host}</p>
						<p class="text-muted-foreground font-mono text-[11px]">{n.addr} · {n.node_id}</p>
					</div>
					<div class="flex flex-wrap items-center gap-2">
						<NodeStateChip state={n.state} online={n.online} power={n.power} id={n.node_id} />
						<AvailChips presence={cur.w?.presence ?? n.presence} blocked={!!cur.w?.blocked} cap={cur.w?.cap ?? null} />
					</div>
					{#if n.state_reason}<p class="text-muted-foreground">{n.state_reason}{n.state_by ? ` · ${n.state_by}` : ''}</p>{/if}
					<dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5">
						<dt class="text-muted-foreground">Seat</dt><dd>{n.lab_id != null && n.x != null ? `${labName(n.lab_id)} · column ${n.x + 1}, row ${(n.y ?? 0) + 1}` : 'Unplaced'}</dd>
						<dt class="text-muted-foreground">CPU</dt><dd class="min-w-0 break-words">{n.inventory?.cpu_model || '—'}</dd>
						<dt class="text-muted-foreground">Threads</dt><dd class="num">{count(cur.w?.threads ?? n.inventory?.cores_logical)}</dd>
						<dt class="text-muted-foreground">RAM</dt><dd class="num">{bytes(n.inventory?.ram_bytes ?? cur.w?.mem_total)}</dd>
						<dt class="text-muted-foreground">Load</dt><dd class="num">{cur.w ? `${pct(cur.w.cpu)} · ${rate(cur.w.rate)} · ${count(cur.w.chunks)} chunks` : '—'}</dd>
						<dt class="text-muted-foreground">Class</dt><dd class="min-w-0 break-words font-mono text-[11px]">{n.class || '—'}</dd>
						<dt class="text-muted-foreground">Pools</dt>
						<dd>{#if n.pools.length}<span class="flex flex-wrap gap-1">{#each n.pools as p (p)}<span class="border-line rounded-md border px-1.5 py-0.5 text-[11px]">{p}</span>{/each}</span>{:else}—{/if}</dd>
						<dt class="text-muted-foreground">Labels</dt>
						<dd><LabelChips labels={n.labels} onremove={canAct ? (k) => void unlabel(n.node_id, k) : undefined} /></dd>
					</dl>
					{#if canAct && !selected.includes(n.node_id)}
						<NodeActions ids={[n.node_id]} />
					{/if}
				</section>
			{:else if !selected.length}
				<p class="text-muted-foreground text-[13px]">Click a PC in the list to see its details here. Tick boxes to act on several at once.</p>
			{/if}
			{#if canAct}
				<div class="border-line border-t pt-3">
					<SelectorBar bind:value={sel} bind:matched label="Select PCs by rule" id="nodes-sel">
						{#snippet actions()}
							<button type="button" class="border-line hover:bg-raised self-start rounded-lg border px-3 py-1.5 text-[13px] disabled:opacity-50" disabled={!matched?.length} onclick={() => (selected = [...matched!])}>
								Select {matched?.length ?? ''}
							</button>
						{/snippet}
					</SelectorBar>
					{#if matched}<p class="text-muted-foreground mt-1 text-xs">The list is narrowed to this rule while it matches.</p>{/if}
				</div>
			{/if}
		</div>
	</Panel>
</Page>
