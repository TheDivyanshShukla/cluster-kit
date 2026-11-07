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
	import { autoArrange, AVAIL, boxCells, cellAt, cpuTone, STATE_LOOK, SWATCH, toItems, type Overlay } from '#lib/map.ts';
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
	let drag = $state<{ id: string; x: number; y: number; target: { lab: number; x: number; y: number; taken: boolean } | null } | null>(null);
	// the seat under the pointer while dragging (null off the map)
	function seatAt(cx: number, cy: number, id: string) {
		const svg = document.elementFromPoint(cx, cy)?.closest('svg[data-lab]');
		const lab = svg && live.labs.find((l) => l.id === Number(svg.getAttribute('data-lab')));
		const c = lab && cellAt(svg.getBoundingClientRect(), cx, cy, lab.w, lab.h);
		if (!lab || !c) return null;
		const taken = items.some((i) => i.node.lab_id === lab.id && i.node.x === c.x && i.node.y === c.y && i.node.node_id !== id);
		return { lab: lab.id, ...c, taken };
	}
	// pointer moves are folded to one update per frame: the page re-renders at most 60 times a second while dragging
	let raf = 0;
	let lastMove: PointerEvent | null = null;
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
		lastMove = e;
		if (!raf) raf = requestAnimationFrame(() => ((raf = 0), lastMove && step(lastMove)));
	}
	function step(e: PointerEvent) {
		if (!rec) return;
		if (!rec.moved && Math.hypot(e.clientX - rec.x, e.clientY - rec.y) < 5) return;
		rec.moved = true;
		if (rec.nodeId) {
			if (editing) drag = { id: rec.nodeId, x: e.clientX, y: e.clientY, target: seatAt(e.clientX, e.clientY, rec.nodeId) };
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
		cancelAnimationFrame(raf);
		raf = 0;
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
			const t = seatAt(e.clientX, e.clientY, d.id);
			if (t) return void place(d.id, t.lab, t.x, t.y);
			if (document.elementFromPoint(e.clientX, e.clientY)?.closest('[data-unplace]')) return void unplace([d.id]);
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

	// Optimistic: the tile moves at once (and glides, FloorMap), the head confirms after. A taken seat swaps the two PCs;
	// the one sitting there goes to the moved PC's old seat (or to Unplaced if it had none).
	async function place(id: string, lab_id: number, x: number, y: number) {
		err = '';
		const n = live.nodes[id];
		if (!n || (n.lab_id === lab_id && n.x === x && n.y === y)) return;
		const occ = Object.values(live.nodes).find((o) => o.node_id !== id && o.lab_id === lab_id && o.x === x && o.y === y);
		const had = n.lab_id != null && n.x != null && n.y != null;
		const back = had ? { lab_id: n.lab_id!, x: n.x!, y: n.y! } : { lab_id: null };
		live.nodes[id] = { ...n, lab_id, x, y };
		if (occ) live.nodes[occ.node_id] = { ...occ, lab_id: n.lab_id, x: had ? n.x : null, y: had ? n.y : null };
		try {
			if (occ) await api.place(occ.node_id, { lab_id: null }); // frees the seat first
			live.nodes[id] = (await api.place(id, { lab_id, x, y })).node;
			if (occ) live.nodes[occ.node_id] = (await api.place(occ.node_id, back)).node;
		} catch (e) {
			err = e instanceof ApiError ? (e.status === 409 ? 'That seat was just taken. Try another one.' : e.message) : 'Could not place the PC.';
			// put back what the head really has
			for (const k of occ ? [id, occ.node_id] : [id]) api.node(k).then((r) => (live.nodes[k] = r.node)).catch(() => {});
		}
	}
	async function unplace(ids: string[]) {
		err = '';
		busy = true;
		try {
			for (const id of ids) if (live.nodes[id]) live.nodes[id] = { ...live.nodes[id], lab_id: null, x: null, y: null };
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
			{#snippet sw(tone: keyof typeof SWATCH, glyph: string, label: string)}
				<li class="inline-flex items-center gap-1.5"><span class="grid size-4 place-items-center rounded-[4px] border text-[9px] leading-none font-semibold {SWATCH[tone]}" aria-hidden="true">{glyph}</span>{label}</li>
			{/snippet}
			<div class="flex flex-wrap items-center justify-between gap-x-6 gap-y-2">
				<ul class="text-muted-foreground flex flex-wrap items-center gap-x-3.5 gap-y-1.5 text-xs" aria-label="Legend">
					{#if overlay === 'state'}
						{#each Object.entries(STATE_LOOK) as [st, v] (st)}{@render sw(v.tone, v.glyph, st)}{/each}
					{:else if overlay === 'cpu'}
						{#each [[10, 'under 25 %'], [40, '25 to 60'], [70, '60 to 85'], [90, 'over 85']] as const as [c, t] (t)}{@render sw(cpuTone(c), '', t)}{/each}
					{:else if overlay === 'avail'}
						{#each Object.values(AVAIL) as a (a.label)}{@render sw(a.tone, a.glyph, a.label)}{/each}
					{:else}
						{@render sw('busy', '▶', 'working')}{@render sw('idle', '', 'idle')}
					{/if}
					<li class="border-line ml-1 border-l pl-3.5">faded: offline</li>
					<li class="inline-flex items-center gap-1"><Moon class="size-3.5" aria-hidden="true" />asleep</li>
					<li class="text-warn inline-flex items-center gap-1"><Sunrise class="size-3.5" aria-hidden="true" />waking</li>
				</ul>
				{#if Object.keys(live.nodes).length}<p class="num text-muted-foreground text-xs font-medium" aria-label="PCs by power state">{fleetWords(counts)}</p>{/if}
			</div>
			{#if editing}
				<div class="border-primary/40 bg-primary/10 flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border px-3 py-2 text-[13px]" role="status">
					<Pencil class="text-primary size-4 shrink-0" aria-hidden="true" />
					<p class="min-w-0 flex-1">Drag a PC to a seat. Drop it on another PC to swap them, or on <span class="font-medium">Unplaced</span> to take it off the map. Arrow keys move the focused PC.</p>
					<button type="button" class="{btnPrimary} shrink-0" onclick={() => ((edit = false), (selected = []))}>Done</button>
				</div>
			{/if}
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
							target={drag?.target?.lab === lab.id ? drag.target : null}
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
				{#if unplaced.length === 0}
					<p class="text-muted-foreground text-xs">Every PC has a seat.</p>
				{:else}
					<ul class="grid max-h-80 gap-1 overflow-y-auto {drag ? 'outline-primary/50 rounded-lg outline-2 outline-dashed' : ''}">
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
					<details class="border-line mt-3 border-t pt-3">
					<summary class="text-muted-foreground hover:text-foreground cursor-pointer text-xs">Place by seat number</summary>
					<form class="mt-2 grid gap-2" onsubmit={placeAt}>
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
					</details>
				{/if}
			
		</div>
	</Panel>
</Page>

{#if drag}
	<div class="bg-primary text-primary-foreground pointer-events-none fixed z-50 rounded-lg px-2.5 py-1 text-xs font-medium shadow-lg" style="left:{drag.x + 12}px;top:{drag.y + 12}px">
		{byId[drag.id]?.node.host}
	</div>
{/if}
