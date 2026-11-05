<script lang="ts" module>
	export interface ChartPoint {
		x: number;
		y: number;
		/** range of the repeats */
		lo?: number;
		hi?: number;
	}
	export interface ChartSeries {
		name: string;
		color: string;
		/** stroke-dasharray for predictions and reference lines */
		dash?: string;
		/** draw the points (measured data); otherwise a bare line */
		dots?: boolean;
		pts: ChartPoint[];
	}
</script>

<script lang="ts">
	import { Check, Clipboard, Download } from '@lucide/svelte';
	import { copyPng, download, fmtNum, niceTicks, toCsv } from '#lib/chart.ts';

	// Line chart with hairline axes, units on both axes, range bars for repeats, exact values on hover,
	// "Download CSV" and "Copy as PNG" (docs/DESIGN.md section 9).
	interface Props {
		title: string;
		xLabel: string;
		yLabel: string;
		series: ChartSeries[];
		/** log2 x axis (PC counts) */
		xlog?: boolean;
		yMax?: number;
		yFmt?: (v: number) => string;
		/** file name stem for the exports */
		file: string;
	}
	let { title, xLabel, yLabel, series, xlog = false, yMax, yFmt = (v) => fmtNum(v), file }: Props = $props();

	let w = $state(300), h = $state(200);
	let svg = $state<SVGSVGElement>();
	let hover = $state<number | null>(null);
	let copied = $state<'' | 'ok' | 'fail'>('');
	const M = { l: 42, r: 10, t: 8, b: 34 };

	const xs = $derived([...new Set(series.flatMap((s) => s.pts.map((p) => p.x)))].sort((a, b) => a - b));
	const xMin = $derived(xs[0] ?? 0);
	const xMax = $derived(xs.at(-1) ?? 1);
	const top = $derived(yMax ?? Math.max(...series.flatMap((s) => s.pts.map((p) => p.hi ?? p.y)), 1e-9) * 1.05);
	const yt = $derived(niceTicks(0, top, Math.max(2, Math.floor(h / 48))));
	const yTop = $derived(Math.max(top, yt.at(-1) ?? top));
	const px = (x: number) => {
		const [a, b, v] = xlog ? [Math.log2(xMin), Math.log2(xMax), Math.log2(x)] : [xMin, xMax, x];
		return M.l + (b === a ? (w - M.l - M.r) / 2 : ((v - a) / (b - a)) * (w - M.l - M.r));
	};
	const py = (y: number) => M.t + (1 - y / yTop) * (h - M.t - M.b);
	// x ticks: every measured x when few, else the powers of two (or nice ticks on a linear axis)
	const xt = $derived(xs.length <= Math.max(4, Math.floor(w / 46)) ? xs : xlog ? xs.filter((x) => Number.isInteger(Math.log2(x))) : niceTicks(xMin, xMax, Math.floor(w / 70)));
	const path = (s: ChartSeries) => s.pts.map((p, i) => `${i ? 'L' : 'M'}${px(p.x).toFixed(1)},${py(p.y).toFixed(1)}`).join('');

	function move(e: PointerEvent) {
		const r = svg!.getBoundingClientRect();
		const mx = e.clientX - r.left;
		hover = xs.reduce<number | null>((best, x) => (best === null || Math.abs(px(x) - mx) < Math.abs(px(best) - mx) ? x : best), null);
	}
	const at = (s: ChartSeries, x: number) => s.pts.find((p) => p.x === x);
	const tipLeft = $derived(hover !== null && px(hover) > w / 2);

	function csv() {
		const rows = series.flatMap((s) => s.pts.map((p) => [s.name, p.x, p.y, p.lo ?? '', p.hi ?? '']));
		download(`${file}.csv`, toCsv(['series', xLabel, yLabel, 'min', 'max'], rows));
	}
	async function copy() {
		try {
			await copyPng(svg!);
			copied = 'ok';
		} catch {
			copied = 'fail';
		}
		setTimeout(() => (copied = ''), 2000);
	}
</script>

<figure class="flex min-h-0 min-w-0 flex-1 flex-col">
	<figcaption class="mb-1 flex min-h-7 items-center gap-1.5">
		<span class="truncate text-[13px] font-semibold">{title}</span>
		<span class="no-print ml-auto flex shrink-0 items-center gap-1">
			<button type="button" class="text-muted-foreground hover:text-foreground hover:bg-raised inline-flex h-7 items-center gap-1 rounded-md px-1.5 text-xs max-sm:h-9" onclick={csv}><Download class="size-3.5" aria-hidden="true" />CSV</button>
			<button type="button" class="text-muted-foreground hover:text-foreground hover:bg-raised inline-flex h-7 items-center gap-1 rounded-md px-1.5 text-xs max-sm:h-9" onclick={copy} aria-label="Copy as PNG">
				{#if copied === 'ok'}<Check class="text-ok size-3.5" aria-hidden="true" />Copied{:else}<Clipboard class="size-3.5" aria-hidden="true" />{copied === 'fail' ? 'Not allowed' : 'PNG'}{/if}
			</button>
		</span>
	</figcaption>
	<div class="chart-box relative min-h-0 flex-1" bind:clientWidth={w} bind:clientHeight={h}>
		<svg bind:this={svg} viewBox="0 0 {w} {h}" role="img" aria-label="{title}: {yLabel} against {xLabel}" class="absolute inset-0 size-full touch-pan-y select-none" onpointermove={move} onpointerleave={() => (hover = null)} font-family="inherit">
			{#each yt as t (t)}
				<line x1={M.l} x2={w - M.r} y1={py(t)} y2={py(t)} stroke="var(--line)" stroke-width="0.5" />
				<text x={M.l - 5} y={py(t) + 3.5} text-anchor="end" font-size="11" fill="var(--muted-foreground)" class="num">{yFmt(t)}</text>
			{/each}
			<line x1={M.l} x2={M.l} y1={M.t} y2={h - M.b} stroke="var(--muted-foreground)" stroke-width="0.75" />
			<line x1={M.l} x2={w - M.r} y1={h - M.b} y2={h - M.b} stroke="var(--muted-foreground)" stroke-width="0.75" />
			{#each xt as t (t)}
				<line x1={px(t)} x2={px(t)} y1={h - M.b} y2={h - M.b + 3} stroke="var(--muted-foreground)" stroke-width="0.75" />
				<text x={px(t)} y={h - M.b + 14} text-anchor="middle" font-size="11" fill="var(--muted-foreground)" class="num">{fmtNum(t)}</text>
			{/each}
			<text x={(M.l + w - M.r) / 2} y={h - 4} text-anchor="middle" font-size="11" fill="var(--muted-foreground)">{xLabel}</text>
			<text transform="translate(11 {(M.t + h - M.b) / 2}) rotate(-90)" text-anchor="middle" font-size="11" fill="var(--muted-foreground)">{yLabel}</text>
			{#if hover !== null}<line x1={px(hover)} x2={px(hover)} y1={M.t} y2={h - M.b} stroke="var(--muted-foreground)" stroke-width="0.5" stroke-dasharray="2 2" />{/if}
			{#each series as s (s.name)}
				{#if s.pts.length > 1}<path d={path(s)} fill="none" stroke={s.color} stroke-width="1.5" stroke-dasharray={s.dash} />{/if}
				{#if s.dots}
					{#each s.pts as p (p.x)}
						{#if p.lo !== undefined && p.hi !== undefined && p.hi > p.lo}
							<line x1={px(p.x)} x2={px(p.x)} y1={py(p.lo)} y2={py(p.hi)} stroke={s.color} stroke-width="1" />
							<line x1={px(p.x) - 3} x2={px(p.x) + 3} y1={py(p.lo)} y2={py(p.lo)} stroke={s.color} stroke-width="1" />
							<line x1={px(p.x) - 3} x2={px(p.x) + 3} y1={py(p.hi)} y2={py(p.hi)} stroke={s.color} stroke-width="1" />
						{/if}
						<circle cx={px(p.x)} cy={py(p.y)} r={hover === p.x ? 4 : 2.75} fill={s.color} />
					{/each}
				{/if}
			{/each}
		</svg>
		{#if hover !== null}
			<div class="bg-surface border-line pointer-events-none absolute top-1 z-10 rounded-md border px-2 py-1 text-xs shadow-sm" style="{tipLeft ? 'right' : 'left'}:{tipLeft ? w - px(hover) + 8 : px(hover) + 8}px">
				<div class="text-muted-foreground num">{fmtNum(hover)} {xLabel.replace(/ \(.*/, '')}</div>
				{#each series as s (s.name)}
					{@const p = at(s, hover)}
					{#if p}
						<div class="flex items-center gap-1.5 whitespace-nowrap">
							<span class="inline-block h-0.5 w-3" style="background:{s.color}"></span>{s.name}
							<span class="num ml-auto pl-2 font-medium">{yFmt(p.y)}</span>{#if p.lo !== undefined && p.hi !== undefined && p.hi > p.lo}<span class="num text-muted-foreground">({yFmt(p.lo)} to {yFmt(p.hi)})</span>{/if}
						</div>
					{/if}
				{/each}
			</div>
		{/if}
	</div>
	<div class="text-muted-foreground mt-1 flex flex-wrap gap-x-3 text-[11px]">
		{#each series as s (s.name)}<span class="inline-flex items-center gap-1"><span class="inline-block h-0 w-3 border-t-[1.5px]" style="border-color:{s.color};border-style:{s.dash ? 'dashed' : 'solid'}"></span>{s.name}</span>{/each}
	</div>
</figure>

<style>
	@media print {
		.chart-box {
			min-height: 6.5cm;
		}
	}
</style>
