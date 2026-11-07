<script lang="ts">
	import { Moon, Sunrise, TriangleAlert } from '@lucide/svelte';
	import { fade } from 'svelte/transition';
	import type { LabRow } from '#lib/api/schema.ts';
	import { identifying } from '#lib/format.ts';
	import { live } from '#lib/live.svelte.ts';
	import { clock } from '#lib/topology.ts';
	import { CELL, labelSize, look, tileLabel, type Item, type Overlay } from '#lib/map.ts';

	interface Props {
		lab: LabRow;
		items: Item[]; // only the PCs placed in this lab
		overlay: Overlay;
		selected?: string[];
		flash?: Record<string, number>;
		editing?: boolean;
		/** wall mode: no focus stops, bigger labels */
		wall?: boolean;
		/** box-select rectangle in cell units */
		box?: { x0: number; y0: number; x1: number; y1: number } | null;
		dragging?: string | null;
		/** the seat under a dragged PC: free (move there) or taken (swap) */
		target?: { x: number; y: number; taken: boolean } | null;
		ondown?: (e: PointerEvent, labId: number, nodeId: string | null) => void;
		onkey?: (e: KeyboardEvent, item: Item) => void;
	}
	let { lab, items, overlay, selected = [], flash = {}, editing = false, wall = false, box = null, dragging = null, target = null, ondown, onkey }: Props = $props();

	// waking tiles show how long ago the packet went out; one clock tick per second only while some PC is waking
	let now = $state(Date.now());
	const anyWaking = $derived(items.some((i) => i.node.power === 'waking' || identifying(i.node, now)));
	$effect(() => {
		if (!anyWaking) return;
		const t = setInterval(() => (now = Date.now()), 1000);
		return () => clearInterval(t);
	});
	const wakeSecs = (id: string) => Math.max(0, Math.floor((now - (live.wakeSince[id] ?? now)) / 1000));
	const cells = $derived(Array.from({ length: lab.w * lab.h }, (_, i) => ({ x: i % lab.w, y: Math.floor(i / lab.w) })));
	const placed = $derived(items.filter((i) => i.node.x != null && i.node.y != null && i.node.x < lab.w && i.node.y < lab.h));
	const recent = (id: string) => flash[id] != null && Date.now() - flash[id] < 1500;
	const idOf = (e: Event) => (e.target as Element).closest<SVGElement>('[data-node]')?.dataset.node ?? null;
</script>

<svg
	data-lab={lab.id}
	viewBox="0 0 {lab.w * CELL} {lab.h * CELL}"
	role="group"
	aria-label="{lab.name}, {lab.w} by {lab.h} seats"
	class="block {wall ? 'h-full' : 'h-auto'} w-full select-none {editing ? 'touch-none' : ''}"
	style={wall ? undefined : `max-width:${lab.w * CELL * 1.6}px`}
	onpointerdown={(e) => ondown?.(e, lab.id, idOf(e))}
>
	{#each cells as c (`${c.x},${c.y}`)}
		<rect
			x={c.x * CELL + 1}
			y={c.y * CELL + 1}
			width={CELL - 2}
			height={CELL - 2}
			rx="8"
			class="seat {editing ? 'fill-raised/40 stroke-muted-foreground/40' : 'fill-raised/25 stroke-line/60'}"
			stroke-width="1"
			stroke-dasharray={editing ? '3 3' : undefined}
		/>
	{/each}
	{#each placed as it (it.node.node_id)}
		{@const l = look(it, overlay, wakeSecs(it.node.node_id))}
		{@const sel = selected.includes(it.node.node_id)}
		<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
		<g
			data-node={it.node.node_id}
			style="transform:translate({it.node.x! * CELL}px,{it.node.y! * CELL}px)"
			in:fade={{ duration: 150 }}
			role="button"
			tabindex={wall ? undefined : 0}
			aria-pressed={sel}
			aria-label="{it.node.host}, {it.node.state}{it.node.online ? '' : ', offline'}, {l.label}"
			class="tile {editing ? 'cursor-grab' : 'cursor-pointer'} {l.dim ? 'opacity-40' : ''} {dragging === it.node.node_id ? 'opacity-25' : ''}"
			onkeydown={(e) => onkey?.(e, it)}
		>
			<title>{it.node.host}: {it.node.state}{it.node.online ? '' : ' (offline)'}, {l.label}</title>
			<rect x="2" y="2" width={CELL - 4} height={CELL - 4} rx="8" class="{l.fill} {l.stroke}" stroke-width="1.25" stroke-dasharray={l.dash ? '4 2' : undefined} />
			<text x={l.power ? 5 : CELL / 2} y={l.glyph || l.power || !it.node.online ? 16 : CELL / 2 + 3.5} text-anchor={l.power ? 'start' : 'middle'} font-size={labelSize(tileLabel(it.node), l.power ? 8.5 : 10)} font-weight="500" class="fill-foreground/85 pointer-events-none">{tileLabel(it.node)}</text>
			<text x={CELL / 2} y={it.node.online ? 32 : 28} text-anchor="middle" font-size={it.node.online ? 12 : 10} font-weight="600" class="{l.text} pointer-events-none">{l.glyph}</text>
				{#if identifying(it.node, now)}<rect x="1" y="1" width={CELL - 2} height={CELL - 2} rx="7" fill="none" class="ck-pulse stroke-primary pointer-events-none" stroke-width="4" />{/if}
				{#if l.power === 'waking'}<rect x="2" y="2" width={CELL - 4} height={CELL - 4} rx="6" fill="none" class="ck-pulse stroke-warn pointer-events-none" stroke-width="1.5" />{/if}
			{#if l.power === 'asleep'}<Moon x={CELL - 14} y="3" width="11" height="11" class="stroke-muted-foreground pointer-events-none" />
			{:else if l.power === 'waking'}<Sunrise x={CELL - 14} y="3" width="11" height="11" class="stroke-warn pointer-events-none" />
			{:else if l.power === 'failed'}<TriangleAlert x={CELL - 14} y="3" width="11" height="11" class="stroke-bad pointer-events-none" />{/if}
			{#if l.power}
				<text x={CELL / 2} y="40" text-anchor="middle" font-size="7.5" font-weight="700" class="fill-foreground pointer-events-none">{l.power === 'asleep' ? 'ASLEEP' : l.power === 'waking' ? 'WAKING' : 'NO WAKE'}</text>
				{#if l.power !== 'asleep'}<text x={CELL / 2} y="29" text-anchor="middle" font-size="9" class="num fill-muted-foreground pointer-events-none">{clock(wakeSecs(it.node.node_id))}</text>{/if}
			{:else if !it.node.online}
				<text x={CELL / 2} y="40" text-anchor="middle" font-size="7.5" font-weight="700" class="fill-foreground pointer-events-none">OFFLINE</text>
			{/if}
			{#if sel}<rect x="0.5" y="0.5" width={CELL - 1} height={CELL - 1} rx="7" fill="none" class="stroke-foreground pointer-events-none" stroke-width="3" />{/if}
			{#key flash[it.node.node_id]}
				{#if recent(it.node.node_id)}
					<rect x="2" y="2" width={CELL - 4} height={CELL - 4} rx="6" class="ck-flash fill-none stroke-foreground pointer-events-none" stroke-width="4" />
				{/if}
			{/key}
		</g>
	{/each}
	{#if target}
		<g class="pointer-events-none" style="transform:translate({target.x * CELL}px,{target.y * CELL}px)">
			<rect x="1" y="1" width={CELL - 2} height={CELL - 2} rx="9" class={target.taken ? 'fill-warn/15 stroke-warn' : 'fill-primary/15 stroke-primary'} stroke-width="2" />
			{#if target.taken}<text x={CELL / 2} y={CELL - 5} text-anchor="middle" font-size="7.5" font-weight="700" class="fill-warn">SWAP</text>{/if}
		</g>
	{/if}
	{#if box}
		<rect
			x={Math.min(box.x0, box.x1) * CELL}
			y={Math.min(box.y0, box.y1) * CELL}
			width={Math.abs(box.x1 - box.x0) * CELL}
			height={Math.abs(box.y1 - box.y0) * CELL}
			class="fill-primary/15 stroke-primary pointer-events-none"
			stroke-width="1.5"
			stroke-dasharray="4 3"
		/>
	{/if}
</svg>

<style>
	.ck-flash {
		animation: ck-flash 0.9s ease-out forwards;
	}
	@media (prefers-reduced-motion: no-preference) {
		.ck-pulse {
			transform-box: fill-box;
			transform-origin: center;
			animation: ck-pulse 2.4s ease-out infinite;
		}
	}
	@keyframes ck-pulse {
		from {
			opacity: 0.9;
			transform: scale(1);
		}
		to {
			opacity: 0;
			transform: scale(1.3);
		}
	}
	@keyframes ck-flash {
		from {
			opacity: 1;
		}
		to {
			opacity: 0;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.ck-flash {
			display: none;
		}
	}
	.tile {
		transform-box: view-box;
	}
	@media (prefers-reduced-motion: no-preference) {
		.tile {
			transition:
				transform 220ms cubic-bezier(0.2, 0.8, 0.2, 1),
				opacity 150ms;
		}
	}
	.seat {
		transition: fill 120ms;
	}
	g:focus-visible {
		outline: 2px solid var(--ring);
		outline-offset: 0;
	}
</style>
