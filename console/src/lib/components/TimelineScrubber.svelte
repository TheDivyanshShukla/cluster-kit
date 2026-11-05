<script lang="ts">
	import type { EventRow } from '#lib/api/schema.ts';
	import { railKind, type Tone } from '#lib/canvas/effects.ts';
	import { bucket, history, SPAN_MS } from '#lib/canvas/history.ts';
	import { live } from '#lib/live.svelte.ts';
	import { btnGhost } from '#lib/ui.ts';

	// The last 30 minutes: sparkline of busy PCs and tasks/s, event ticks by kind, and a slider. Dragging into the past
	// replays (read-only); `at` is null while live.
	let { events, at = $bindable(null) }: { events: EventRow[]; at?: number | null } = $props();
	const W = 600, H = 48, N = 120;
	let pos = $state(1000);
	// the history ring is not reactive: re-read it whenever a cluster snapshot arrives
	const snap = $derived.by(() => {
		void live.cluster;
		const t = Date.now(), last = history.samples[history.samples.length - 1];
		return { now: t, from: t - SPAN_MS, series: bucket(history.samples, t - SPAN_MS, t, N), tasks: Math.round(last?.tasks ?? 0), busy: last?.busy ?? 0 };
	});
	const now = $derived(snap.now);
	const from = $derived(snap.from);
	const series = $derived(snap.series);
	const line = (v: number[]) => {
		const m = Math.max(1, ...v);
		return v.map((y, i) => `${((i / (N - 1)) * W).toFixed(1)},${(H - 3 - (y / m) * (H - 8)).toFixed(1)}`).join(' ');
	};
	const stroke: Record<Tone, string> = { ok: 'stroke-ok', bad: 'stroke-bad', warn: 'stroke-warn', busy: 'stroke-primary', idle: 'stroke-muted-foreground' };
	const ticks = $derived(events.filter((e) => e.ts_ms >= from && e.ts_ms <= now && !railKind(e.kind).key.match(/^(user|other)$/)).map((e) => ({ id: e.id, x: ((e.ts_ms - from) / SPAN_MS) * W, cls: stroke[railKind(e.kind).tone] })));
	const lastTasks = $derived(snap.tasks);
	const lastBusy = $derived(snap.busy);
	// while replaying, the slider keeps pointing at the same moment as the window slides
	$effect(() => {
		if (at != null) pos = Math.max(0, Math.min(1000, Math.round(((at - from) / SPAN_MS) * 1000)));
	});
	function slide(e: Event) {
		pos = Number((e.currentTarget as HTMLInputElement).value);
		at = pos >= 995 ? null : Math.round(from + (pos / 1000) * SPAN_MS);
	}
	function goLive() {
		pos = 1000;
		at = null;
	}
	const stamp = (t: number) => new Date(t).toLocaleTimeString('en-GB');
</script>

<div class="bg-surface border-line rounded-lg border px-3 py-2" role="group" aria-label="Timeline, last 30 minutes">
	<div class="text-muted-foreground mb-1 flex flex-wrap items-center justify-between gap-x-4 gap-y-1 text-xs">
		<span class="inline-flex items-center gap-3">
			<span class="inline-flex items-center gap-1.5"><span class="bg-primary inline-block h-0.5 w-4" aria-hidden="true"></span>tasks/s <span class="num text-foreground">{lastTasks}</span></span>
			<span class="inline-flex items-center gap-1.5"><span class="border-ok inline-block w-4 border-t-2 border-dashed" aria-hidden="true"></span>busy PCs <span class="num text-foreground">{lastBusy}</span></span>
		</span>
		<span class="inline-flex items-center gap-2">
			<span class="num {at != null ? 'text-warn font-medium' : ''}" aria-live="polite">{at != null ? `Replay ${stamp(at)}` : 'Live'}</span>
			<button type="button" class="{btnGhost} h-9 px-3 text-xs" disabled={at == null} onclick={goLive}>Live</button>
		</span>
	</div>
	<div class="relative">
		<svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" class="block h-12 w-full" aria-hidden="true">
			<line x1="0" x2={W} y1={H - 1} y2={H - 1} class="stroke-line" stroke-width="1" vector-effect="non-scaling-stroke" />
			{#each ticks as t (t.id)}<line x1={t.x} x2={t.x} y1="0" y2={H} class={t.cls} stroke-width="1.5" opacity="0.55" vector-effect="non-scaling-stroke" />{/each}
			<polyline points={line(series.busy)} fill="none" class="stroke-ok" stroke-width="1.5" stroke-dasharray="4 3" vector-effect="non-scaling-stroke" />
			<polyline points={line(series.tasks)} fill="none" class="stroke-primary" stroke-width="1.75" vector-effect="non-scaling-stroke" />
			{#if at != null}<line x1={(pos / 1000) * W} x2={(pos / 1000) * W} y1="0" y2={H} class="stroke-warn" stroke-width="2" vector-effect="non-scaling-stroke" />{/if}
		</svg>
		<input type="range" min="0" max="1000" step="1" value={pos} oninput={slide} aria-label="Replay time: drag into the past, right end is live" class="absolute inset-0 h-full w-full cursor-ew-resize opacity-0" />
	</div>
	<div class="text-muted-foreground flex justify-between text-[11px]"><span>30 min ago</span><span>now</span></div>
</div>
