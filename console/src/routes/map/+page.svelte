<script lang="ts">
	import { LayoutGrid, Moon, Pencil, Sunrise, Wand2, MapPinOff, Settings2, Map as MapIcon } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import FloorMap from '#lib/components/FloorMap.svelte';
	import LabsEditor from '#lib/components/LabsEditor.svelte';
	import NodeActions from '#lib/components/NodeActions.svelte';
	import NodeStateChip from '#lib/components/NodeStateChip.svelte';
	import Segmented from '#lib/components/Segmented.svelte';
	import { pct } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { autoArrange, AVAIL, boxCells, cellAt, STATE_LOOK, toItems, type Overlay } from '#lib/map.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { fleetWords, powerCounts } from '#lib/topology.ts';
	import { btnGhost, btnPrimary, field } from '#lib/ui.ts';

	let overlay = $state<Overlay>('state');
	let view = $state<'map' | 'table'>('map');
	let edit = $state(false);
	let selected = $state<string[]>([]);
	let err = $state('');
	let busy = $state(false);

	const canEdit = $derived(atLeast(roleNow(), 'operator'));
	const editing = $derived(edit && canEdit);
	const items = $derived(toItems(live.nodes, live.cluster?.workers ?? []));
	const unplaced = $derived(items.filter((i) => i.node.lab_id == null || i.node.x == null));
	const inLab = (id: number) => items.filter((i) => i.node.lab_id === id && i.node.x != null);
	const counts = $derived(powerCounts(Object.values(live.nodes)));
	const byId = $derived(Object.fromEntries(items.map((i) => [i.node.node_id, i])));
	const labName = (id: number | null) => live.labs.find((l) => l.id === id)?.name ?? '';

	// ---- pointer interaction: click selects, drag moves (edit mode), dragging empty floor box-selects
	type Rec = { x: number; y: number; labId: number | null; nodeId: string | null; shift: boolean; moved: boolean };
	let rec: Rec | null = null;
	let drag = $state<{ id: string; x: number; y: number } | null>(null);
	let boxPx = $state<{ labId: number; x0: number; y0: number; x1: number; y1: number } | null>(null);
	const svgOf = (labId: number) => document.querySelector<SVGElement>(`svg[data-lab="${labId}"]`)?.getBoundingClientRect();
	const toggle = (id: string, multi: boolean) =>
		(selected = multi ? (selected.includes(id) ? selected.filter((s) => s !== id) : [...selected, id]) : selected.length === 1 && selected[0] === id ? [] : [id]);

	function down(e: PointerEvent, labId: number | null, nodeId: string | null) {
		if (e.button !== 0) return;
		rec = { x: e.clientX, y: e.clientY, labId, nodeId, shift: e.shiftKey, moved: false };
	}
	function move(e: PointerEvent) {
		if (!rec) return;
		if (!rec.moved && Math.hypot(e.clientX - rec.x, e.clientY - rec.y) < 5) return;
		rec.moved = true;
		if (rec.nodeId) {
			if (editing) drag = { id: rec.nodeId, x: e.clientX, y: e.clientY };
		} else if (rec.labId != null) {
			const r = svgOf(rec.labId), lab = live.labs.find((l) => l.id === rec!.labId);
			if (r && lab)
				boxPx = {
					labId: lab.id,
					x0: ((rec.x - r.left) / r.width) * lab.w, y0: ((rec.y - r.top) / r.height) * lab.h,
					x1: ((e.clientX - r.left) / r.width) * lab.w, y1: ((e.clientY - r.top) / r.height) * lab.h
				};
		}
	}
	async function up(e: PointerEvent) {
		const r = rec, d = drag, b = boxPx;
		rec = null; drag = null; boxPx = null;
		if (!r) return;
		if (!r.moved) {
			if (r.nodeId) return toggle(r.nodeId, r.shift);
			// click on an empty seat with one unplaced PC selected places it there
			const lab = live.labs.find((l) => l.id === r.labId), rect = r.labId != null ? svgOf(r.labId) : null;
			const one = selected.length === 1 ? byId[selected[0]] : null;
			const c = lab && rect ? cellAt(rect, e.clientX, e.clientY, lab.w, lab.h) : null;
			if (editing && lab && c && one && one.node.lab_id == null) return void place(one.node.node_id, lab.id, c.x, c.y);
			if (!r.shift) selected = [];
			return;
		}
		if (d && editing) {
			const el = document.elementFromPoint(e.clientX, e.clientY);
			const svg = el?.closest('svg[data-lab]');
			if (svg) {
				const lab = live.labs.find((l) => l.id === Number(svg.getAttribute('data-lab')));
				const c = lab && cellAt(svg.getBoundingClientRect(), e.clientX, e.clientY, lab.w, lab.h);
				if (lab && c) return void place(d.id, lab.id, c.x, c.y);
			} else if (el?.closest('[data-unplace]')) return void unplace([d.id]);
		} else if (b) {
			const lab = live.labs.find((l) => l.id === b.labId), rect = svgOf(b.labId);
			if (!lab || !rect) return;
			const cells = new Set(boxCells(rect, [r.x, r.y], [e.clientX, e.clientY], lab.w, lab.h).map((c) => `${c.x},${c.y}`));
			const ids = inLab(lab.id).filter((i) => cells.has(`${i.node.x},${i.node.y}`)).map((i) => i.node.node_id);
			selected = r.shift ? [...new Set([...selected, ...ids])] : ids;
		}
	}

	function key(e: KeyboardEvent, it: { node: { node_id: string; lab_id: number | null; x: number | null; y: number | null } }) {
		const n = it.node;
		if (e.key === 'Enter' || e.key === ' ') {
			e.preventDefault();
			return toggle(n.node_id, true);
		}
		const dir = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
		if (!dir || !editing || n.lab_id == null || n.x == null || n.y == null) return;
		e.preventDefault();
		const lab = live.labs.find((l) => l.id === n.lab_id);
		const x = n.x + dir[0], y = n.y + dir[1];
		if (lab && x >= 0 && y >= 0 && x < lab.w && y < lab.h) void place(n.node_id, lab.id, x, y);
	}

	async function place(id: string, lab_id: number, x: number, y: number) {
		err = '';
		try {
			live.nodes[id] = (await api.place(id, { lab_id, x, y })).node;
		} catch (e) {
			err = e instanceof ApiError ? (e.status === 409 ? 'That seat is taken. Pick an empty one.' : e.message) : 'Could not place the PC.';
		}
	}
	async function unplace(ids: string[]) {
		err = '';
		busy = true;
		try {
			for (const id of ids) live.nodes[id] = (await api.place(id, { lab_id: null })).node;
			selected = selected.filter((s) => !ids.includes(s));
		} catch (e) {
			err = e instanceof ApiError ? e.message : 'Could not remove the PC from the map.';
		}
		busy = false;
	}

	let arrangeLab = $state<number | null>(null);
	async function arrange() {
		const lab = live.labs.find((l) => l.id === (arrangeLab ?? live.labs[0]?.id));
		if (!lab) return;
		const pool = selected.length ? unplaced.filter((u) => selected.includes(u.node.node_id)) : unplaced;
		const plan = autoArrange(pool, lab.w, lab.h, inLab(lab.id).map((i) => `${i.node.x},${i.node.y}`));
		busy = true;
		for (const p of plan) await place(p.id, lab.id, p.x, p.y);
		busy = false;
		if (!plan.length) err = pool.length ? `${lab.name} has no empty seats.` : 'Every PC is already placed.';
	}

	let at = $state({ id: '', lab: 0, col: 1, row: 1 });
	const placeAt = (e: SubmitEvent) => {
		e.preventDefault();
		const lab = at.lab || live.labs[0]?.id;
		if (at.id && lab) void place(at.id, lab, at.col - 1, at.row - 1);
	};
	const selPlaced = $derived(selected.filter((s) => byId[s]?.node.lab_id != null));
</script>

<svelte:head><title>Floor map · Cluster Kit</title></svelte:head>
<svelte:window onpointermove={move} onpointerup={up} />

<Page
	title="Floor map"
	subtitle="Each tile is a PC, drawn where it sits in the room."
	tabs={[{ id: 'map', label: 'Map' }, { id: 'unplaced', label: 'Unplaced', badge: unplaced.length }]}
	class="sm:grid-cols-[minmax(0,1fr)_15rem] lg:grid-cols-[minmax(0,1fr)_16rem]"
>
	{#snippet actions()}
		<Segmented legend="Colour tiles by" bind:value={overlay} options={[{ value: 'state', label: 'State' }, { value: 'cpu', label: 'CPU' }, { value: 'work', label: 'Working' }, { value: 'avail', label: 'Availability' }]} />
		<Segmented legend="View" bind:value={view} options={[{ value: 'map', label: 'Map' }, { value: 'table', label: 'Table' }]} />
		{#if canEdit}
			<button type="button" class={edit ? btnPrimary : btnGhost} aria-pressed={edit} onclick={() => (edit = !edit)}>
				<Pencil class="size-4" aria-hidden="true" />{edit ? 'Done editing' : 'Edit map'}
			</button>
		{/if}
	{/snippet}

	<Panel title="Map" tab="map" label="Floor map" count={live.labs.length ? `${live.labs.length} lab${live.labs.length === 1 ? '' : 's'}` : undefined}>
		<div class="space-y-3">
			<p class="text-muted-foreground flex flex-wrap gap-x-4 gap-y-1 text-xs" aria-label="Legend">
	{#if overlay === 'state'}
		{#each Object.entries(STATE_LOOK) as [s, v] (s)}<span><span class="text-foreground font-mono text-sm">{v.glyph}</span> {s}</span>{/each}
	{:else if overlay === 'cpu'}
		<span>number = CPU %</span><span>below 25 idle</span><span>25 to 60 normal</span><span>60 to 85 high</span><span>85 up busiest</span>
	{:else if overlay === 'avail'}
		{#each Object.values(AVAIL) as a (a.label)}<span><span class="text-foreground font-mono text-sm">{a.glyph}</span> {a.label}</span>{/each}
	{:else}
		<span><span class="text-foreground font-mono text-sm">▶</span> working</span><span><span class="text-foreground font-mono text-sm">–</span> idle</span>
	{/if}
	<span>OFFLINE = dimmed, not connected</span>
	<span class="inline-flex items-center gap-1"><Moon class="text-foreground size-3.5" aria-hidden="true" />asleep (dashed, faint)</span>
	<span class="inline-flex items-center gap-1"><Sunrise class="text-warn size-3.5" aria-hidden="true" />waking (warn outline)</span>
</p>
			{#if Object.keys(live.nodes).length}<p class="num text-[13px] font-medium" aria-label="PCs by power state">{fleetWords(counts)}</p>{/if}
			{#if selected.length}<NodeActions ids={selected} ondone={() => (selected = [])} />{/if}
			{#if err}<p class="text-bad text-[13px]" role="alert">{err}</p>{/if}
			{#if !live.cluster && !Object.keys(live.nodes).length}
				<div class="bg-raised h-64 animate-pulse rounded-lg" aria-busy="true"></div>
			{:else if view === 'table'}
				<div class="border-line relative overflow-x-auto rounded-lg border">
			<table class="w-full text-left text-[13px]">
				<caption class="sr-only">Every PC with its lab seat, state, CPU and activity</caption>
				<thead class="text-muted-foreground border-line border-b text-xs">
					<tr><th class="px-3 py-2 font-medium">PC</th><th class="px-3 py-2 font-medium">State</th><th class="px-3 py-2 font-medium">Seat</th><th class="px-3 py-2 text-right font-medium">CPU</th><th class="px-3 py-2 font-medium">Activity</th></tr>
				</thead>
				<tbody>
					{#each items as it (it.node.node_id)}
						<tr class="border-line border-b last:border-0">
							<th scope="row" class="px-3 py-2 font-medium">{it.node.host}</th>
							<td class="px-3 py-2"><NodeStateChip state={it.node.state} online={it.node.online} power={it.node.power} id={it.node.node_id} /></td>
							<td class="px-3 py-2">{#if it.node.lab_id != null && it.node.x != null}{labName(it.node.lab_id)}, column {it.node.x + 1}, row {(it.node.y ?? 0) + 1}{:else}<span class="text-muted-foreground">Unplaced</span>{/if}</td>
							<td class="num px-3 py-2 text-right">{it.w ? pct(it.w.cpu) : '—'}</td>
							<td class="px-3 py-2">{it.w ? (it.w.busy ? 'Working' : 'Idle') : '—'}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
			{:else if live.labs.length === 0}
				<EmptyState icon={MapIcon} title="No lab yet" body={canEdit ? 'Create a lab with its width and height, then place your PCs on it.' : 'An operator creates labs and places PCs on them.'}>
					{#snippet action()}{#if canEdit}<button type="button" class={btnPrimary} onclick={() => (edit = true)}>Create a lab</button>{/if}{/snippet}
				</EmptyState>
			{:else}
				<div class="grid gap-5 xl:grid-cols-2">
				{#each live.labs as lab (lab.id)}
					<section class="bg-surface border-line min-w-0 rounded-lg border p-3" aria-label={lab.name}>
						<h2 class="mb-2 flex items-baseline justify-between text-[13px] font-semibold">
							{lab.name}<span class="text-muted-foreground num text-xs font-normal">{inLab(lab.id).length} of {lab.w * lab.h} seats</span>
						</h2>
						<FloorMap
							{lab}
							items={inLab(lab.id)}
							{overlay}
							{selected}
							flash={live.flash}
							{editing}
							dragging={drag?.id ?? null}
							box={boxPx?.labId === lab.id ? boxPx : null}
							ondown={down}
							onkey={key}
						/>
					</section>
				{/each}
			</div>
			{/if}
			{#if editing}
	<details class="bg-surface border-line mt-5 rounded-lg border p-3">
		<summary class="flex cursor-pointer items-center gap-2 text-[13px] font-semibold"><Settings2 class="size-4" aria-hidden="true" />Labs</summary>
		<div class="mt-3"><LabsEditor /></div>
	</details>
{/if}


		</div>
	</Panel>

	<Panel title="Unplaced" tab="unplaced" count={unplaced.length} label="Unplaced PCs">
		<div data-unplace>

				<h2 class="text-[13px] font-semibold">Unplaced <span class="text-muted-foreground num font-normal">({unplaced.length})</span></h2>
				{#if unplaced.length === 0}
					<p class="text-muted-foreground mt-2 text-xs">Every PC has a seat.</p>
				{:else}
					<ul class="mt-2 grid max-h-80 gap-1 overflow-y-auto">
						{#each unplaced as it (it.node.node_id)}
							<li>
								<button
									type="button"
									aria-pressed={selected.includes(it.node.node_id)}
									class="hover:bg-raised aria-pressed:bg-raised aria-pressed:border-foreground flex w-full items-center justify-between gap-2 rounded-lg border border-transparent px-2 py-1.5 text-left text-[13px] {editing ? 'cursor-grab touch-none' : ''} {it.node.online ? '' : 'opacity-50'}"
									onpointerdown={(e) => down(e, null, it.node.node_id)}
									onclick={(e) => e.detail === 0 && toggle(it.node.node_id, true)}
								>
									<span class="truncate">{it.node.host}</span>
									<span class="text-muted-foreground shrink-0 font-mono text-sm" title={it.node.state}>{STATE_LOOK[it.node.state].glyph}</span>
								</button>
							</li>
						{/each}
					</ul>
				{/if}
				{#if editing}
					<p class="text-muted-foreground mt-3 text-xs">Drag a PC onto a seat, or select it and click a seat. On a keyboard, focus a tile and use the arrow keys, or use the form below.</p>
					<div class="border-line mt-3 grid gap-2 border-t pt-3">
						<div class="flex gap-2">
							<select value={arrangeLab ?? live.labs[0]?.id} onchange={(e) => (arrangeLab = Number(e.currentTarget.value))} class="{field} min-w-0 flex-1" aria-label="Lab to fill">
								{#each live.labs as l (l.id)}<option value={l.id}>{l.name}</option>{/each}
							</select>
							<button type="button" class={btnGhost} disabled={busy || !unplaced.length} onclick={arrange}><Wand2 class="size-4" aria-hidden="true" />Auto-arrange</button>
						</div>
						<p class="text-muted-foreground text-xs">Fills empty seats in host-name order{selected.length ? ' with the selected unplaced PCs' : ''}.</p>
						<button type="button" class={btnGhost} disabled={busy || !selPlaced.length} onclick={() => unplace(selPlaced)}>
							<MapPinOff class="size-4" aria-hidden="true" />Remove from map{selPlaced.length ? ` (${selPlaced.length})` : ''}
						</button>
					</div>
					<form class="border-line mt-3 grid gap-2 border-t pt-3" onsubmit={placeAt}>
						<p class="text-[13px] font-medium">Place at</p>
						<select bind:value={at.id} required class={field} aria-label="PC to place">
							<option value="" disabled>PC…</option>
							{#each unplaced as it (it.node.node_id)}<option value={it.node.node_id}>{it.node.host}</option>{/each}
						</select>
						<select value={at.lab || live.labs[0]?.id} onchange={(e) => (at.lab = Number(e.currentTarget.value))} class={field} aria-label="Lab">
							{#each live.labs as l (l.id)}<option value={l.id}>{l.name}</option>{/each}
						</select>
						<div class="grid grid-cols-2 gap-2">
							<label class="text-xs"><span class="text-muted-foreground">Column</span><input type="number" min="1" max="64" bind:value={at.col} class="{field} num mt-0.5" /></label>
							<label class="text-xs"><span class="text-muted-foreground">Row</span><input type="number" min="1" max="64" bind:value={at.row} class="{field} num mt-0.5" /></label>
						</div>
						<button type="submit" class={btnGhost} disabled={!at.id}><LayoutGrid class="size-4" aria-hidden="true" />Place</button>
					</form>
				{/if}
			
		</div>
	</Panel>
</Page>

{#if drag}
	<div class="bg-primary text-primary-foreground pointer-events-none fixed z-50 rounded-lg px-2.5 py-1 text-xs font-medium shadow-lg" style="left:{drag.x + 12}px;top:{drag.y + 12}px">
		{byId[drag.id]?.node.host}
	</div>
{/if}
