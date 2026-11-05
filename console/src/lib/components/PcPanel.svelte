<script lang="ts">
	import { X } from '@lucide/svelte';
	import { api } from '#lib/api/client.ts';
	import type { Candidate, EventRow } from '#lib/api/schema.ts';
	import { hostIndex, idsIn, railText } from '#lib/canvas/effects.ts';
	import { history } from '#lib/canvas/history.ts';
	import NodeActions from '#lib/components/NodeActions.svelte';
	import NodeStateChip from '#lib/components/NodeStateChip.svelte';
	import { ago, bytes, count, duration, pct, rate } from '#lib/format.ts';
	import { live } from '#lib/live.svelte.ts';

	// The canvas side panel for one PC: Info, Activity (30 minute sparkline), Events, Actions.
	interface Props {
		id: string;
		events: EventRow[];
		/** replay is read-only: no actions */
		readonly?: boolean;
		onclose: () => void;
	}
	let { id, events, readonly = false, onclose }: Props = $props();
	const TABS = ['Info', 'Activity', 'Events', 'Actions'] as const;
	let tab = $state<(typeof TABS)[number]>('Info');
	const node = $derived(live.nodes[id]);
	const worker = $derived(live.cluster?.workers.find((w) => w.node_id === id));
	const lab = $derived(live.labs.find((l) => l.id === node?.lab_id)?.name ?? 'no lab');
	const hosts = $derived(hostIndex(Object.values(live.nodes)));
	const mine = $derived(events.filter((e) => idsIn(e, hosts).includes(id)).slice(0, 30));
	const stamp = (t: number) => new Date(t).toLocaleTimeString('en-GB');

	let cand = $state<Candidate | null>(null);
	$effect(() => {
		const me = id;
		cand = null;
		api.candidates().then((r) => (cand = r.candidates.find((c) => c.node_id === me) ?? null)).catch(() => {});
	});

	// activity: per-PC samples kept by the console while it is open
	const W = 300, H = 56;
	const series = $derived.by(() => {
		void live.cluster;
		return history.perNode.get(id) ?? null;
	});
	let hover = $state<number | null>(null);
	const pts = (v: number[], max: number) => v.map((y, i) => `${((i / Math.max(1, v.length - 1)) * W).toFixed(1)},${(H - 3 - (y / (max || 1)) * (H - 8)).toFixed(1)}`).join(' ');
	const maxRate = $derived(Math.max(1e-9, ...(series?.rate ?? [0])));
	function move(e: PointerEvent) {
		const r = (e.currentTarget as SVGElement).getBoundingClientRect();
		hover = Math.min((series?.t.length ?? 1) - 1, Math.max(0, Math.round(((e.clientX - r.left) / r.width) * ((series?.t.length ?? 1) - 1))));
	}
</script>

<div class="flex min-h-0 flex-1 flex-col">
	<div class="border-line flex shrink-0 items-center gap-1 border-b px-2">
		<div role="tablist" aria-label="PC details" class="flex">
			{#each TABS as t (t)}
				<button type="button" role="tab" aria-selected={tab === t} class="px-2.5 py-2 text-xs max-sm:min-h-9 {tab === t ? 'border-primary text-foreground border-b-2 font-medium' : 'text-muted-foreground hover:text-foreground'}" onclick={() => (tab = t)}>{t}</button>
			{/each}
		</div>
		{#if node}<span class="ml-auto"><NodeStateChip state={node.state} online={node.online} power={node.power} id={node.node_id} /></span>{/if}
		<button type="button" class="text-muted-foreground hover:text-foreground inline-flex size-8 items-center justify-center" aria-label="Close" onclick={onclose}><X class="size-4" aria-hidden="true" /></button>
	</div>
	<div class="thin min-h-0 flex-1 overflow-auto p-3 text-[13px]">
		{#if !node}
			<p class="text-muted-foreground">This PC is no longer registered.</p>
		{:else if tab === 'Info'}
			<dl class="grid grid-cols-[6.5rem_1fr] gap-x-3 gap-y-1">
				<dt class="text-muted-foreground">Host</dt><dd class="min-w-0 truncate font-medium" title={node.host}>{node.host}</dd>
				<dt class="text-muted-foreground">Lab</dt><dd>{lab}{node.x != null ? ` · seat ${node.x + 1},${(node.y ?? 0) + 1}` : ''}</dd>
				<dt class="text-muted-foreground">Address</dt><dd class="num min-w-0 truncate font-mono text-xs">{node.addr}</dd>
				<dt class="text-muted-foreground">CPU</dt><dd class="min-w-0 break-words">{node.inventory?.cpu_model || '—'}</dd>
				<dt class="text-muted-foreground">Threads</dt><dd class="num">{count(node.inventory?.cores_logical ?? worker?.threads)} <span class="text-muted-foreground">({count(node.inventory?.cores_physical)} cores)</span></dd>
				<dt class="text-muted-foreground">RAM</dt><dd class="num">{bytes(node.inventory?.ram_bytes ?? worker?.mem_total)}</dd>
				<dt class="text-muted-foreground">OS</dt><dd class="min-w-0 truncate">{node.inventory?.os ?? '—'} <span class="text-muted-foreground">{node.inventory?.arch ?? ''}</span></dd>
				<dt class="text-muted-foreground">Agent</dt><dd class="num">{node.inventory?.ck_version ?? '—'}</dd>
				<dt class="text-muted-foreground">Calibration</dt><dd class="num">{node.calib_mhs != null ? `${node.calib_mhs.toFixed(2)} MH/s` : '—'}</dd>
				<dt class="text-muted-foreground">Now</dt><dd class="num">{worker ? `${pct(worker.cpu)} CPU · ${rate(worker.rate)}` : 'not connected'}</dd>
				{#if node.power !== 'awake' || node.power_since_ms}<dt class="text-muted-foreground">Power</dt><dd class="num">{node.power}{node.power_since_ms ? ` for ${duration(Date.now() - node.power_since_ms)}` : ''}</dd>{/if}
				<dt class="text-muted-foreground">Last seen</dt><dd class="num">{ago(node.last_seen_ms)}</dd>
				{#if node.pools.length}<dt class="text-muted-foreground">Pools</dt><dd class="min-w-0 break-words">{node.pools.join(', ')}</dd>{/if}
				{#if Object.keys(node.labels).length}<dt class="text-muted-foreground">Labels</dt><dd class="min-w-0 break-words font-mono text-xs">{Object.entries(node.labels).map(([k, v]) => `${k}=${v}`).join(' ')}</dd>{/if}
			</dl>
			<h3 class="text-muted-foreground mt-3 mb-1 text-[11px] tracking-wide uppercase">Head score</h3>
			{#if cand}
				<p class="num mb-1">{cand.score.toFixed(0)} <span class="text-muted-foreground">{cand.eligible ? 'eligible' : (cand.why || 'not eligible')}</span></p>
				<dl class="grid grid-cols-[6.5rem_1fr_2.5rem] items-center gap-x-3 gap-y-1">
					{#each Object.entries(cand.parts) as [k, v] (k)}
						<dt class="text-muted-foreground capitalize">{k}</dt>
						<dd class="bg-raised h-1.5 overflow-hidden rounded-full"><span class="bg-primary block h-full" style="width:{Math.min(100, v * (v <= 1 ? 100 : 1))}%"></span></dd>
						<dd class="num text-right text-xs">{v <= 1 ? (v * 100).toFixed(0) : v.toFixed(0)}</dd>
					{/each}
				</dl>
			{:else}<p class="text-muted-foreground text-xs">Not available (needs the operator role) or the PC is not a candidate.</p>{/if}
		{:else if tab === 'Activity'}
			{#if series && series.t.length > 1}
				<svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" class="block h-20 w-full touch-none" role="img" aria-label="Rate and CPU, last 30 minutes" onpointermove={move} onpointerleave={() => (hover = null)}>
					<line x1="0" x2={W} y1={H - 1} y2={H - 1} class="stroke-line" vector-effect="non-scaling-stroke" />
					<polyline points={pts(series.cpu, 100)} fill="none" class="stroke-ok" stroke-width="1.5" stroke-dasharray="4 3" vector-effect="non-scaling-stroke" />
					<polyline points={pts(series.rate, maxRate)} fill="none" class="stroke-primary" stroke-width="1.75" vector-effect="non-scaling-stroke" />
					{#if hover != null}<line x1={(hover / (series.t.length - 1)) * W} x2={(hover / (series.t.length - 1)) * W} y1="0" y2={H} class="stroke-muted-foreground" vector-effect="non-scaling-stroke" />{/if}
				</svg>
				<p class="text-muted-foreground num mt-1 flex justify-between text-xs">
					<span class="inline-flex items-center gap-1"><span class="bg-primary inline-block h-0.5 w-4"></span>rate {hover != null ? rate(series.rate[hover]) : `max ${rate(maxRate)}`}</span>
					<span class="inline-flex items-center gap-1"><span class="border-ok inline-block w-4 border-t-2 border-dashed"></span>CPU {hover != null ? `${series.cpu[hover].toFixed(0)} %` : '0-100 %'}</span>
					<span>{hover != null ? stamp(series.t[hover]) : `${Math.round((series.t.at(-1)! - series.t[0]) / 60000)} min`}</span>
				</p>
			{:else}<p class="text-muted-foreground">The console records this PC's rate and CPU while it is open. Come back in a minute.</p>{/if}
		{:else if tab === 'Events'}
			{#if mine.length}
				<ul class="space-y-1">{#each mine as e (e.id)}<li class="flex gap-2"><span class="text-muted-foreground num shrink-0 text-xs">{stamp(e.ts_ms)}</span><span class="min-w-0 break-words">{railText(e)}</span></li>{/each}</ul>
			{:else}<p class="text-muted-foreground">No recent events for this PC.</p>{/if}
		{:else if readonly}
			<p class="text-muted-foreground">Replay is read-only. Press Live to act on this PC.</p>
		{:else}
			<NodeActions ids={[id]} ondone={onclose} />
		{/if}
	</div>
</div>
