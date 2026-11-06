<script lang="ts">
	import { ChevronDown, Crown, Eye, Hexagon, Maximize, Spline, WifiOff, ZoomIn, ZoomOut } from '@lucide/svelte';
	import { onMount, untrack } from 'svelte';
	import { api } from '#lib/api/client.ts';
	import { jobColor } from '#lib/canvas/colors.ts';
	import { decodeView, encodeView, HEAT_GRADIENT, isLayer, LAYER_LABEL, LAYERS, legendFor, type Layer } from '#lib/canvas/layers.ts';
	import { params, patchQuery } from '#lib/urlstate.svelte.ts';
	import { idsIn, hostIndex } from '#lib/canvas/effects.ts';
	import { moveFocus, type Dir } from '#lib/canvas/focus.ts';
	import { LiveEngine } from '#lib/canvas/engine.ts';
	import { headAt, rewind, type NodeSnap } from '#lib/canvas/replay.ts';
	import { parseSelector } from '#lib/canvas/select.ts';
	import { canvasHeads } from '#lib/hier.ts';
	import { CLS_WORDS, CLASSES, type Cls } from '#lib/canvas/state.ts';
	import type { EventRow } from '#lib/api/schema.ts';
	import { runningJobs } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { count, rate } from '#lib/format.ts';
	import { field } from '#lib/ui.ts';

	interface Props {
		/** selector text; PCs that do not match dim to 15 % */
		filter?: string;
		/** replay: show the fleet as it was at this time (ms), rebuilt from `replayEvents`; null = live */
		replayAt?: number | null;
		replayEvents?: EventRow[];
		/** the wall: no pan/zoom/hover, bigger legend */
		wall?: boolean;
		/** small canvas (Overview): no toolbar, one-line legend */
		compact?: boolean;
		/** layer, view, follow and selection live in the URL (the Live page) */
		urlState?: boolean;
		/** the PC to draw a halo around */
		selected?: string | null;
		onpick?: (id: string | null) => void;
		onmatch?: (n: number, total: number) => void;
	}
	let { filter = '', replayAt = null, replayEvents = [], wall = false, compact = false, urlState = false, selected = null, onpick, onmatch }: Props = $props();

	const bare = $derived(wall || compact);
	// svelte-ignore state_referenced_locally
	let layer = $state<Layer>(urlState && isLayer(params().get('layer')) ? (params().get('layer') as Layer) : 'status');
	// svelte-ignore state_referenced_locally
	let bundles = $state(urlState && params().get('flow') === '1');
	// svelte-ignore state_referenced_locally
	let followJob = $state<number | null>(urlState && Number(params().get('follow')) > 0 ? Number(params().get('follow')) : null);
	let maxRate = $state(0);
	let maxLoss = $state(0);
	let lod = $state<'point' | 'square' | 'card'>('square');
	let losses = $state<Record<string, number>>({});
	let scores = $state<Record<string, number>>({});
	let scoreErr = $state(false);
	let bar = $state<HTMLDivElement>();

	let wrap = $state<HTMLDivElement>();
	let canvas = $state<HTMLCanvasElement>();
	let engine = $state.raw<LiveEngine>();
	let counts = $state<Record<Cls, number>>({ busy: 0, idle: 0, use: 0, cord: 0, asleep: 0, waking: 0, lost: 0 });
	let total = $state(0);
	let tip = $state<{ id: string; x: number; y: number } | null>(null);
	let tokens = $state.raw<LiveEngine['tokens']>();
	let legend = $state<HTMLDivElement>();
	// the legend can be folded away so it never hides a lab; closed by default on a phone
	let legendOpen = $state(typeof matchMedia !== 'function' || !matchMedia('(max-width: 639px)').matches);
	const showLegend = $derived(compact || wall || legendOpen);
	$effect(() => {
		const e = engine, l = legend;
		if (!e || !l) return;
		const ro = new ResizeObserver(() => e.setPad(l.offsetHeight + 12, bar ? bar.offsetHeight + 10 : 4));
		ro.observe(l);
		if (bar) ro.observe(bar);
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
			ha: live.ha, filter: matcher, overrides, headId, losses, scores, hier: canvasHeads(live.hier)
		});
		counts = { ...e.counts };
		maxRate = e.maxRate;
		maxLoss = e.maxLoss;
		lod = e.lod;
		total = e.items.length;
		onmatch?.(e.matched, e.items.length);
	});
	$effect(() => engine?.setLayer(layer));
	$effect(() => engine?.setSelected(selected));
	$effect(() => {
		if (engine) engine.bundles = bundles;
		if (engine) engine.dirty = true;
	});
	$effect(() => {
		engine?.setFollow(followJob);
	});

	// layer data: losses in 24 h from node.lost events, head score from /ha/candidates
	$effect(() => {
		if (layer !== 'reliability') return;
		let dead = false;
		const load = async () => {
			const idx = hostIndex(Object.values(live.nodes));
			const out: Record<string, number> = {};
			let before: number | null = null;
			for (let p = 0; p < 15; p++) {
				const r = await api.events(200, before);
				for (const ev of r.events) if (ev.kind === 'node.lost' && ev.ts_ms > Date.now() - 864e5) for (const id of idsIn(ev, idx).slice(0, 1)) out[id] = (out[id] ?? 0) + 1;
				before = r.next;
				if (before == null || (r.events.at(-1)?.ts_ms ?? 0) < Date.now() - 864e5) break;
			}
			if (!dead) losses = out;
		};
		void load().catch(() => {});
		const t = setInterval(() => void load().catch(() => {}), 60_000);
		return () => ((dead = true), clearInterval(t));
	});
	$effect(() => {
		if (layer !== 'fitness') return;
		let dead = false;
		const load = async () => {
			try {
				const r = await api.candidates();
				if (!dead) (scores = Object.fromEntries(r.candidates.map((c) => [c.node_id, c.score])), (scoreErr = false));
			} catch {
				if (!dead) scoreErr = true;
			}
		};
		void load();
		const t = setInterval(() => void load(), 20_000);
		return () => ((dead = true), clearInterval(t));
	});

	// URL state: layer, zoom + centre, follow, flow bundles
	let restored = false;
	let writeT: ReturnType<typeof setTimeout> | undefined;
	function writeView() {
		clearTimeout(writeT);
		writeT = setTimeout(() => {
			const e = engine;
			if (!urlState || !e) return;
			const c = e.centre();
			patchQuery((p) => {
				if (e.userView) p.set('view', encodeView(e.view.k, c.x, c.y));
				else p.delete('view');
				if (e.follow == null) p.delete('follow');
				else p.set('follow', String(e.follow));
			});
			if (e.follow !== followJob) followJob = e.follow;
		}, 300);
	}
	$effect(() => {
		const e = engine;
		if (!e) return;
		e.onview = writeView;
		return () => (e.onview = null);
	});
	$effect(() => {
		const e = engine;
		if (!e || !urlState || restored || total === 0) return;
		restored = true;
		const v = decodeView(params().get('view'));
		if (v && followJob == null) e.setView(v.k, v.cx, v.cy);
	});
	$effect(() => {
		if (!urlState) return;
		const l = layer, b = bundles;
		untrack(() => patchQuery((p) => {
			if (l === 'status') p.delete('layer');
			else p.set('layer', l);
			if (b) p.set('flow', '1');
			else p.delete('flow');
		}));
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
		const m = engine?.miniAt(local(ev).x, local(ev).y);
		if (m) return engine!.centerOn(m.x, m.y);
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

	// ---- keyboard: the canvas is one tab stop; arrows move between PCs, Enter opens what a click opens.
	// aria-activedescendant points at the hidden option list below; the ring is drawn on the canvas.
	let kbFocus = $state<string | null>(null);
	let hasFocus = $state(false);
	const KEYS: Record<string, Dir> = { ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down' };
	const kbItems = $derived(engine && total + counts.busy + counts.lost + counts.asleep >= 0 ? engine.items.slice(0, 300) : []); // items is mutated in place: re-read when the counts move
	const kbPcs = () => (engine?.scene.pcs ?? []).slice(0, 300);
	const optId = (id: string) => `ck-pc-${id.replace(/[^\w-]/g, '_')}`;
	$effect(() => engine?.setFocused(hasFocus && !wall ? kbFocus : null));
	function reveal(id: string) {
		const e = engine, i = e?.scene.index.get(id);
		if (!e || i == null) return;
		const s = e.screenOf(id)!;
		if (s.x < 16 || s.y < e.padTop + 8 || s.x > e.w - 16 || s.y > e.h - e.padBottom - 8) e.centerOn(e.scene.pcs[i].x, e.scene.pcs[i].y);
	}
	function key(ev: KeyboardEvent) {
		if (wall || !engine || ev.altKey || ev.ctrlKey || ev.metaKey) return;
		const pcs = kbPcs();
		let next: string | null | undefined;
		if (KEYS[ev.key]) next = moveFocus(pcs, kbFocus, KEYS[ev.key]);
		else if (ev.key === 'Home') next = pcs[0]?.id;
		else if (ev.key === 'End') next = pcs.at(-1)?.id;
		else if ((ev.key === 'Enter' || ev.key === ' ') && kbFocus) {
			ev.preventDefault();
			return onpick?.(kbFocus);
		} else return;
		ev.preventDefault();
		if (next) (kbFocus = next), reveal(next);
	}
	function focusIn() {
		hasFocus = true;
		if (!kbFocus || !engine?.scene.index.has(kbFocus)) kbFocus = selected && engine?.scene.index.has(selected) ? selected : (kbPcs()[0]?.id ?? null);
	}

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
		const m = engine.metricsOf(tip.id);
		const extra = layer === 'load' && m?.cpu != null ? `CPU ${Math.round(m.cpu)} %` : layer === 'reliability' ? `${m?.losses ?? 0} losses in 24 h` : layer === 'fitness' && m?.score != null ? `Head score ${m.score.toFixed(0)}` : layer === 'throughput' && m?.rate != null ? rate(m.rate) : null;
		return { extra, host: it.node.host, lab, state: CLS_WORDS[cls], job: job == null ? null : `#${job} ${live.jobs[job]?.name ?? ''}`.trim(), rate: it.w && it.w.rate > 0 ? rate(it.w.rate) : null, last: ev ? ev.detail || ev.kind : null };
	});
	const jobs = $derived([...runningJobs(live.cluster)].sort((a, b) => a.id - b.id).slice(0, 8));
	const summaryNow = $derived(`Live system: ${total} PCs, ${counts.busy} working, ${counts.asleep} asleep, ${counts.lost} lost.`);
	// screen readers hear the counts at most every 20 s, not on every tick
	let summary = $state('');
	let said = 0;
	$effect(() => {
		const s = summaryNow;
		const wait = Math.max(0, said + 20_000 - Date.now());
		const t = setTimeout(() => ((summary = s), (said = Date.now())), wait);
		return () => clearTimeout(t);
	});
	// text alternative to the canvas: a keyboard-reachable list of PCs (first 200, hidden until it has focus)
	let listOpen = $state(false);
	const pcList = $derived(listOpen ? engine?.items.slice(0, 200) ?? [] : []);
	const sw: Record<Cls, string> = {
		busy: 'bg-primary border-primary',
		idle: 'bg-surface border-idle',
		use: 'bg-surface border-warn border-2',
		cord: 'bg-surface border-idle bg-[linear-gradient(135deg,transparent_42%,var(--idle)_42%,var(--idle)_58%,transparent_58%)]',
		asleep: 'bg-idle/35 border-idle border-dashed',
		waking: 'bg-warn/40 border-warn motion-safe:animate-pulse',
		lost: 'bg-bad border-bad'
	};
	const lg = $derived(legendFor(layer, { rate: maxRate, losses: maxLoss }, rate));
	const fs = $derived(wall ? 'text-[max(0.7rem,0.95vw)]' : 'text-[11px]');
</script>

<div bind:this={wrap} class="relative size-full min-h-0 overflow-hidden rounded-lg">
	<!-- svelte-ignore a11y_no_interactive_element_to_noninteractive_role -->
	<canvas
		bind:this={canvas}
		class="absolute inset-0 size-full {wall ? '' : 'cursor-grab touch-none active:cursor-grabbing'} {frozen ? 'opacity-50 grayscale' : ''} transition-[opacity,filter]"
		role="application"
		tabindex={wall || compact ? -1 : 0}
		aria-label="Live map of labs and PCs. Arrow keys move between PCs, Enter opens the PC."
		aria-activedescendant={kbFocus && hasFocus ? optId(kbFocus) : undefined}
		onkeydown={key}
		onfocus={focusIn}
		onblur={() => (hasFocus = false)}
		onpointerdown={down}
		onpointermove={move}
		onpointerup={up}
		onpointercancel={up}
		onpointerleave={() => (tip = null)}
		ondblclick={() => !wall && engine?.resetView()}
	></canvas>
	<p class="sr-only" aria-live="polite">{summary}</p>
	{#if !wall && !compact}
		<ul class="sr-only" role="listbox" aria-label="PCs on the live map">
			{#each kbItems as it (it.node.node_id)}
				<li id={optId(it.node.node_id)} role="option" aria-selected={kbFocus === it.node.node_id}>{it.node.host}, {it.node.online ? it.node.state : 'offline'}</li>
			{/each}
		</ul>
	{/if}
	{#if !wall && !compact}
		<ul class="sr-only" role="listbox" aria-label="PCs on the live map">
			{#each kbItems as it (it.node.node_id)}
				<li id={optId(it.node.node_id)} role="option" aria-selected={kbFocus === it.node.node_id}>{it.node.host}, {it.node.online ? it.node.state : 'offline'}</li>
			{/each}
		</ul>
	{/if}
	{#if !wall}
		<details class="bg-surface border-line sr-only z-30 rounded-lg border focus-within:not-sr-only focus-within:absolute focus-within:top-2 focus-within:left-2" ontoggle={(e) => (listOpen = e.currentTarget.open)}>
			<summary class="cursor-pointer px-3 py-1.5 text-xs font-medium">PCs as a list ({total})</summary>
			<ul class="thin max-h-64 overflow-auto py-1 text-xs">
				{#each pcList as it (it.node.node_id)}
					<li><button type="button" class="hover:bg-raised flex w-full gap-2 px-3 py-1 text-left" onclick={() => onpick?.(it.node.node_id)}><span class="font-medium">{it.node.host}</span><span class="text-muted-foreground">{it.node.online ? it.node.state : 'offline'}</span></button></li>
				{/each}
			</ul>
		</details>
	{/if}

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
			{#if tipInfo.extra}<p class="num">{tipInfo.extra}</p>{/if}
			{#if tipInfo.last}<p class="text-muted-foreground truncate">Last: {tipInfo.last}</p>{/if}
		</div>
	{/if}

	{#if !bare}
		<div bind:this={bar} class="thin absolute top-2 right-2 left-2 flex items-center gap-1.5 overflow-x-auto pb-0.5">
			<label class="sm:hidden">
				<span class="sr-only">Layer</span>
				<select class="{field} h-9 w-auto" bind:value={layer}>{#each LAYERS as l (l)}<option value={l}>{LAYER_LABEL[l]}</option>{/each}</select>
			</label>
			<div class="bg-surface/90 border-line max-sm:hidden inline-flex shrink-0 rounded-lg border p-0.5" role="group" aria-label="Layer">
				{#each LAYERS as l (l)}
					<button type="button" aria-pressed={layer === l} class="rounded-md px-2.5 py-1 text-xs whitespace-nowrap {layer === l ? 'bg-primary/15 text-foreground font-medium' : 'text-muted-foreground hover:text-foreground'}" onclick={() => (layer = l)}>{LAYER_LABEL[l]}</button>
				{/each}
			</div>
			<div class="bg-surface/90 border-line ml-auto inline-flex shrink-0 items-center rounded-lg border p-0.5" role="group" aria-label="View">
				<button type="button" class="hover:bg-raised grid size-8 place-items-center rounded-md" aria-label="Zoom in" onclick={() => engine?.zoomAt(engine.w / 2, engine.h / 2, 1.5)}><ZoomIn class="size-4" aria-hidden="true" /></button>
				<button type="button" class="hover:bg-raised grid size-8 place-items-center rounded-md" aria-label="Zoom out" onclick={() => engine?.zoomAt(engine.w / 2, engine.h / 2, 1 / 1.5)}><ZoomOut class="size-4" aria-hidden="true" /></button>
				<button type="button" class="hover:bg-raised inline-flex h-8 items-center gap-1 rounded-md px-2 text-xs" onclick={() => ((followJob = null), engine?.resetView())}><Maximize class="size-4" aria-hidden="true" /><span class="max-md:sr-only">Fit all</span></button>
				<label class="max-md:hidden"><span class="sr-only">Fit lab</span>
					<select class="hover:bg-raised h-8 rounded-md bg-transparent px-1 text-xs" onchange={(e) => { const v = Number(e.currentTarget.value); e.currentTarget.value = ''; if (v >= 0) engine?.fitLab(v); }}>
						<option value="">Fit lab…</option>
						{#each engine?.scene.labs ?? [] as l (l.id)}<option value={l.id}>{l.name}</option>{/each}
					</select>
				</label>
				<label class="max-md:hidden"><span class="sr-only">Follow job</span>
					<select class="hover:bg-raised h-8 max-w-28 rounded-md bg-transparent px-1 text-xs" value={followJob ?? ''} onchange={(e) => (followJob = e.currentTarget.value ? Number(e.currentTarget.value) : null)}>
						<option value="">Follow job…</option>
						{#each jobs as j (j.id)}<option value={j.id}>#{j.id} {j.name}</option>{/each}
					</select>
				</label>
				<button type="button" aria-pressed={bundles} class="grid size-8 place-items-center rounded-md {bundles ? 'bg-primary/15 text-foreground' : 'hover:bg-raised'}" aria-label="Flow bundles" title="Flow bundles: one band per lab, width = its share of the work" onclick={() => (bundles = !bundles)}><Spline class="size-4" aria-hidden="true" /></button>
			</div>
		</div>
	{/if}

	<div bind:this={legend} class="bg-surface/90 border-line pointer-events-none absolute bottom-2 left-2 {showLegend ? 'right-2' : ''} flex items-center gap-x-3 gap-y-1 rounded-lg border px-2.5 py-1.5 {fs} {showLegend ? 'sm:right-auto sm:max-w-[min(100%-1rem,44rem)]' : 'right-auto left-2'} flex-wrap" aria-label="Legend">
		{#if !compact && !wall}<button type="button" class="pointer-events-auto text-muted-foreground hover:text-foreground inline-flex min-h-6 items-center gap-1" aria-expanded={legendOpen} onclick={() => (legendOpen = !legendOpen)} title={legendOpen ? 'Hide the legend' : 'Show the legend'}><ChevronDown class="size-3.5 transition-transform {legendOpen ? '' : '-rotate-90'}" aria-hidden="true" />Legend</button>{/if}
		{#if showLegend}
		{#if lg.kind === 'scale'}
			<span class="text-muted-foreground inline-flex items-center gap-2">
				<span class="num">{lg.lo}</span><span class="inline-block h-2.5 w-28 rounded-sm" style="background:{HEAT_GRADIENT}" aria-hidden="true"></span><span class="num">{lg.hi}</span> {lg.unit}
				<span class="inline-flex items-center gap-1"><span class="bg-idle/25 border-idle inline-block size-2.5 rounded-sm border" aria-hidden="true"></span>unknown</span>
				{#if layer === 'fitness' && scoreErr}<span class="text-warn">{lg.note}</span>{/if}
			</span>
		{/if}
		{#if lg.kind === 'items' || lg.kind === 'scale'}
			{#each CLASSES as c (c)}
				{#if (counts[c] > 0 || (!compact && (c === 'busy' || c === 'idle'))) && !(lg.kind === 'scale' && c === 'cord')}
					<span class="text-muted-foreground inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-[3px] border {sw[c]}" aria-hidden="true"></span>{CLS_WORDS[c]} <span class="num text-foreground">{count(counts[c])}</span></span>
				{/if}
			{/each}
		{/if}
		{#if !compact}
			<span class="text-muted-foreground inline-flex items-center gap-1"><Crown class="size-3" aria-hidden="true" />head</span>
			<span class="text-muted-foreground inline-flex items-center gap-1"><Eye class="size-3" aria-hidden="true" />standby</span>
			{#if live.hier?.labs.some((l) => l.head)}<span class="text-muted-foreground inline-flex items-center gap-1"><Hexagon class="text-primary size-3" aria-hidden="true" />lab head (ring = lease done; dashed = cut off)</span>{/if}
		{/if}
		{#if tokens && !compact && (lg.kind === 'jobs' || layer === 'status')}
			{#each jobs as j, i (j.id)}
				<span class="text-muted-foreground inline-flex min-w-0 items-center gap-1.5"><span class="inline-block size-2.5 shrink-0 rounded-[3px]" style="background:{jobColor(tokens, i)}" aria-hidden="true"></span><span class="max-w-32 truncate">#{j.id} {j.name}</span></span>
			{/each}
		{/if}
		{#if !compact && lod !== 'point' && !wall}<span class="text-muted-foreground max-md:hidden">ring = CPU busy · stripe = job</span>{/if}
		{/if}
	</div>
</div>
