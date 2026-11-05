<script lang="ts">
	import { Crown, Eye, WifiOff } from '@lucide/svelte';
	import { onMount } from 'svelte';
	import { jobColor } from '#lib/canvas/colors.ts';
	import { idsIn, hostIndex } from '#lib/canvas/effects.ts';
	import { LiveEngine } from '#lib/canvas/engine.ts';
	import { headAt, rewind, type NodeSnap } from '#lib/canvas/replay.ts';
	import { parseSelector } from '#lib/canvas/select.ts';
	import { CLS_WORDS, CLASSES, type Cls } from '#lib/canvas/state.ts';
	import type { EventRow } from '#lib/api/schema.ts';
	import { runningJobs } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { count, rate } from '#lib/format.ts';

	interface Props {
		/** selector text; PCs that do not match dim to 15 % */
		filter?: string;
		/** replay: show the fleet as it was at this time (ms), rebuilt from `replayEvents`; null = live */
		replayAt?: number | null;
		replayEvents?: EventRow[];
		/** the wall: no pan/zoom/hover, bigger legend */
		wall?: boolean;
		onpick?: (id: string | null) => void;
		onmatch?: (n: number, total: number) => void;
	}
	let { filter = '', replayAt = null, replayEvents = [], wall = false, onpick, onmatch }: Props = $props();

	let wrap = $state<HTMLDivElement>();
	let canvas = $state<HTMLCanvasElement>();
	let engine = $state.raw<LiveEngine>();
	let counts = $state<Record<Cls, number>>({ busy: 0, idle: 0, use: 0, cord: 0, asleep: 0, waking: 0, lost: 0 });
	let total = $state(0);
	let tip = $state<{ id: string; x: number; y: number } | null>(null);
	let tokens = $state.raw<LiveEngine['tokens']>();
	let legend = $state<HTMLDivElement>();
	$effect(() => {
		const e = engine, l = legend;
		if (!e || !l) return;
		const ro = new ResizeObserver(() => e.setPad(l.offsetHeight + 12));
		ro.observe(l);
		return () => ro.disconnect();
	});

	const frozen = $derived(live.link === 'reconnecting');
	const matcher = $derived.by(() => {
		const f = filter.trim();
		if (!f) return null;
		const m = parseSelector(f);
		return typeof m === 'string' ? null : m;
	});
	const hostsNow = $derived(hostIndex(Object.values(live.nodes)));
	const headNow = $derived.by(() => {
		const p = live.events.find((e) => e.kind === 'ha.promote');
		return p ? (idsIn(p, hostsNow)[0] ?? null) : null;
	});
	const overrides = $derived.by(() => {
		if (replayAt == null) return null;
		const cur: Record<string, NodeSnap> = {};
		for (const n of Object.values(live.nodes)) cur[n.node_id] = { online: n.online, power: n.power, state: n.state };
		return rewind(cur, replayEvents, replayAt, hostsNow);
	});
	const headId = $derived(replayAt == null ? headNow : headAt(replayEvents, replayAt, hostsNow));

	onMount(() => {
		const e = new LiveEngine(canvas!, wrap!);
		engine = e;
		tokens = e.tokens;
		if (import.meta.env.DEV) (window as unknown as { __ckLive: LiveEngine }).__ckLive = e;
		const size = () => {
			const r = wrap!.getBoundingClientRect();
			e.resize(r.width, r.height, window.devicePixelRatio || 1);
		};
		size();
		const ro = new ResizeObserver(size);
		ro.observe(wrap!);
		const theme = () => {
			e.retheme(wrap!);
			tokens = e.tokens;
		};
		const mo = new MutationObserver(theme);
		mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'class'] });
		const mq = matchMedia('(prefers-color-scheme: dark)');
		mq.addEventListener('change', theme);
		let raf = 0;
		const loop = (t: number) => {
			e.frame(t);
			raf = requestAnimationFrame(loop);
		};
		raf = requestAnimationFrame(loop);
		return () => {
			cancelAnimationFrame(raf);
			ro.disconnect();
			mo.disconnect();
			mq.removeEventListener('change', theme);
			engine = undefined;
		};
	});

	// data in: one pass per cluster snapshot, node change, filter change or scrub
	$effect(() => {
		const e = engine;
		if (!e || frozen) return;
		e.update({
			nodes: live.nodes, labs: live.labs, workers: live.cluster?.workers ?? [], jobs: runningJobs(live.cluster),
			ha: live.ha, filter: matcher, overrides, headId
		});
		counts = { ...e.counts };
		total = e.items.length;
		onmatch?.(e.matched, e.items.length);
	});
	// events in (oldest first so ripples keep order)
	$effect(() => {
		const e = engine;
		if (!e || frozen) return;
		for (const ev of [...live.events].reverse()) e.setEvent(ev);
	});

	export function flash(ids: string[]) {
		engine?.flash(ids);
	}
	export function resetView() {
		engine?.resetView();
	}

	// ---- pointer: drag to pan, wheel / pinch to zoom, double-click resets, click picks
	const pts = new Map<number, { x: number; y: number }>();
	let moved = 0;
	let pinch = 0;
	const local = (ev: PointerEvent | WheelEvent) => {
		const r = canvas!.getBoundingClientRect();
		return { x: ev.clientX - r.left, y: ev.clientY - r.top };
	};
	function down(ev: PointerEvent) {
		if (wall) return;
		canvas!.setPointerCapture(ev.pointerId);
		pts.set(ev.pointerId, local(ev));
		moved = 0;
		pinch = pts.size === 2 ? dist() : 0;
	}
	const dist = () => {
		const [a, b] = [...pts.values()];
		return Math.hypot(a.x - b.x, a.y - b.y) || 1;
	};
	function move(ev: PointerEvent) {
		if (wall || !engine) return;
		const p = local(ev);
		if (pts.has(ev.pointerId)) {
			const prev = pts.get(ev.pointerId)!;
			pts.set(ev.pointerId, p);
			if (pts.size === 2) {
				const d = dist();
				const [a, b] = [...pts.values()];
				engine.zoomAt((a.x + b.x) / 2, (a.y + b.y) / 2, d / (pinch || d));
				pinch = d;
				moved += 10;
			} else {
				engine.panBy(p.x - prev.x, p.y - prev.y);
				moved += Math.abs(p.x - prev.x) + Math.abs(p.y - prev.y);
			}
			tip = null;
		} else if (ev.pointerType === 'mouse') {
			const id = engine.hit(p.x, p.y);
			tip = id ? { id, x: p.x, y: p.y } : null;
		}
	}
	function up(ev: PointerEvent) {
		if (wall) return;
		const was = pts.has(ev.pointerId);
		pts.delete(ev.pointerId);
		if (was && moved < 5 && engine) {
			const p = local(ev);
			const id = engine.hit(p.x, p.y);
			onpick?.(id);
			if (id && ev.pointerType !== 'mouse') tip = { id, x: p.x, y: p.y };
			else if (!id) tip = null;
		}
		pinch = 0;
	}
	function wheel(ev: WheelEvent) {
		if (wall || !engine) return;
		ev.preventDefault();
		const p = local(ev);
		engine.zoomAt(p.x, p.y, Math.exp(-ev.deltaY * 0.0015));
	}
	$effect(() => {
		const c = canvas;
		if (!c) return;
		c.addEventListener('wheel', wheel, { passive: false });
		return () => c.removeEventListener('wheel', wheel);
	});

	// ---- tooltip content
	const tipInfo = $derived.by(() => {
		if (!tip || !engine) return null;
		const i = engine.scene.index.get(tip.id);
		const it = engine.items.find((x) => x.node.node_id === tip!.id);
		if (i == null || !it) return null;
		const cls = engine.cls[i];
		const job = engine.jobOf[i];
		const lab = live.labs.find((l) => l.id === it.node.lab_id)?.name ?? 'unassigned';
		const ev = live.events.find((e) => idsIn(e, hostsNow).includes(tip!.id));
		return { host: it.node.host, lab, state: CLS_WORDS[cls], job: job == null ? null : `#${job} ${live.jobs[job]?.name ?? ''}`.trim(), rate: it.w && it.w.rate > 0 ? rate(it.w.rate) : null, last: ev ? ev.detail || ev.kind : null };
	});
	const jobs = $derived([...runningJobs(live.cluster)].sort((a, b) => a.id - b.id).slice(0, 8));
	const summary = $derived(`Live system: ${total} PCs, ${counts.busy} working, ${counts.asleep} asleep, ${counts.lost} lost.`);
	const sw: Record<Cls, string> = {
		busy: 'bg-primary border-primary',
		idle: 'bg-surface border-idle',
		use: 'bg-surface border-warn border-2',
		cord: 'bg-surface border-idle bg-[linear-gradient(135deg,transparent_42%,var(--idle)_42%,var(--idle)_58%,transparent_58%)]',
		asleep: 'bg-idle/35 border-idle border-dashed',
		waking: 'bg-warn/40 border-warn motion-safe:animate-pulse',
		lost: 'bg-bad border-bad'
	};
	const fs = $derived(wall ? 'text-[max(0.7rem,0.95vw)]' : 'text-[11px]');
</script>

<div bind:this={wrap} class="relative size-full min-h-0 overflow-hidden rounded-lg">
	<canvas
		bind:this={canvas}
		class="absolute inset-0 size-full {wall ? '' : 'cursor-grab touch-none active:cursor-grabbing'} {frozen ? 'opacity-50 grayscale' : ''} transition-[opacity,filter]"
		aria-hidden="true"
		onpointerdown={down}
		onpointermove={move}
		onpointerup={up}
		onpointercancel={up}
		onpointerleave={() => (tip = null)}
		ondblclick={() => !wall && engine?.resetView()}
	></canvas>
	<p class="sr-only" aria-live="polite">{summary}</p>

	{#if !engine || (!live.cluster && total === 0)}
		<div class="absolute inset-0 grid grid-cols-2 gap-3 p-4" aria-busy="true">
			<div class="bg-raised animate-pulse rounded-lg"></div><div class="bg-raised animate-pulse rounded-lg"></div>
		</div>
	{:else if total === 0}
		<div class="absolute inset-0 grid place-items-center p-6 text-center">
			<p class="text-muted-foreground max-w-xs text-[13px]">No PCs yet: add one with a join code. <a class="text-primary underline" href="/nodes">Open Nodes</a></p>
		</div>
	{/if}

	{#if frozen}
		<p class="bg-surface/90 border-line text-warn absolute top-2 left-1/2 flex -translate-x-1/2 items-center gap-1.5 rounded-full border px-3 py-1 text-xs font-medium" role="status"><WifiOff class="size-3.5" aria-hidden="true" />Reconnecting…</p>
	{/if}
	{#if replayAt != null}
		<p class="bg-surface/90 border-line text-warn absolute top-2 left-2 rounded-full border px-3 py-1 text-xs font-medium" role="status">Replay · read-only</p>
	{/if}

	{#if tip && tipInfo && !wall}
		<div
			class="bg-surface border-line pointer-events-none absolute z-10 max-w-60 rounded-lg border px-2.5 py-1.5 text-xs shadow-lg"
			style="left:{Math.min(tip.x + 14, (wrap?.clientWidth ?? 300) - 200)}px;top:{Math.min(tip.y + 14, (wrap?.clientHeight ?? 200) - 90)}px"
			role="tooltip"
		>
			<p class="font-semibold">{tipInfo.host}</p>
			<p class="text-muted-foreground">{tipInfo.lab} · {tipInfo.state}</p>
			{#if tipInfo.job}<p class="truncate">Job {tipInfo.job}</p>{/if}
			{#if tipInfo.rate}<p class="num">{tipInfo.rate}</p>{/if}
			{#if tipInfo.last}<p class="text-muted-foreground truncate">Last: {tipInfo.last}</p>{/if}
		</div>
	{/if}

	<div bind:this={legend} class="bg-surface/90 border-line pointer-events-none absolute right-2 bottom-2 left-2 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-lg border px-2.5 py-1.5 {fs} sm:right-auto sm:max-w-[min(100%-1rem,44rem)]" aria-label="Legend">
		{#each CLASSES as c (c)}
			{#if counts[c] > 0 || c === 'busy' || c === 'idle'}
				<span class="text-muted-foreground inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full border {sw[c]}" aria-hidden="true"></span>{CLS_WORDS[c]} <span class="num text-foreground">{count(counts[c])}</span></span>
			{/if}
		{/each}
		<span class="text-muted-foreground inline-flex items-center gap-1"><Crown class="size-3" aria-hidden="true" />head</span>
		<span class="text-muted-foreground inline-flex items-center gap-1"><Eye class="size-3" aria-hidden="true" />standby</span>
		{#if tokens}
			{#each jobs as j, i (j.id)}
				<span class="text-muted-foreground inline-flex min-w-0 items-center gap-1.5"><span class="inline-block size-2.5 shrink-0 rounded-full" style="background:{jobColor(tokens, i)}" aria-hidden="true"></span><span class="max-w-32 truncate">#{j.id} {j.name}</span></span>
			{/each}
		{/if}
	</div>
</div>
