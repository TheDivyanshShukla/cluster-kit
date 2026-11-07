<script lang="ts">
	import { ArrowRightLeft, Crown, Eye, LoaderCircle, ShieldAlert, ShieldCheck, UserPlus, X } from '@lucide/svelte';
	import { cubicOut } from 'svelte/easing';
	import { Tween } from 'svelte/motion';
	import { fly } from 'svelte/transition';
	import MakeHeadDialog from '#lib/components/MakeHeadDialog.svelte';
	import { duration } from '#lib/format.ts';
	import { hasQuorum } from '#lib/ha.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { live, loadHaCfg } from '#lib/live.svelte.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnGhost } from '#lib/ui.ts';
	import { labPills, nextCandidate, pillWords, promotedBetween, recruitSlots, recruitWords, stageOf, standbyStatus, standbyViews, type StandbyView } from '#lib/topology.ts';

	// Who holds the cluster (docs/DESIGN.md section 3). mini: Overview. full: Settings. wall: the projector page.
	let { size = 'full' }: { size?: 'mini' | 'full' | 'wall' } = $props();

	const h = $derived(live.ha);
	const headHost = $derived(h?.head ? (live.nodes[h.head.node_id]?.host ?? h.head.node_id) : '');
	const stage = $derived(stageOf(h?.role, live.handover != null));
	const views = $derived(standbyViews(h?.standbys ?? [], live.nodes, live.labs));
	const reduce = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;

	// What the standbys were before the head was lost: while it is gone the feed shows none, and #1 is about to take over.
	let snap = $state.raw<StandbyView[]>([]);
	let known: Set<string> | null = null;
	let joined = $state<Record<string, true>>({});
	$effect.pre(() => {
		if (!h || stage !== 'steady') return;
		snap = views;
		if (known) for (const s of views) if (!known.has(s.id)) joined[s.id] = true;
		known = new Set(views.map((s) => s.id));
	});
	const shown = $derived(stage === 'steady' ? views : snap);
	const limit = $derived(size === 'mini' ? 3 : 5);
	const rows = $derived(shown.slice(0, limit));

	// The recruit slot: whenever fewer standbys exist than the target says.
	$effect(() => {
		void h?.epoch;
		void loadHaCfg();
	});
	const slots = $derived(stage === 'steady' ? recruitSlots(live.haCfg?.target_standbys, views.length) : 0);
	const cand = $derived(nextCandidate(live.haCandidates, live.nodes, views.map((s) => s.id)));
	const pills = $derived(labPills(Object.values(live.nodes), live.labs, views));

	// Failover glide: the first standby's card slides into the head slot once (600 ms), the old head card fades out.
	let headEl = $state<HTMLElement>();
	let topoEl = $state<HTMLElement>();
	const glide = new Tween(0, { duration: 600, easing: cubicOut });
	let animating = $state(false);
	let from = $state({ dx: 0, dy: 0 });
	let prevEpoch: number | null = null;
	$effect.pre(() => {
		const e = h?.epoch ?? null;
		const first = topoEl?.querySelector('[data-first]');
		if (promotedBetween(prevEpoch, e) && !reduce && headEl && first) {
			const a = headEl.getBoundingClientRect(), b = first.getBoundingClientRect();
			from = { dx: b.left - a.left, dy: b.top - a.top };
			animating = true;
			void glide.set(1, { duration: 0 }).then(() => glide.set(0)).then(() => (animating = false));
		}
		prevEpoch = e;
	});

	const canSwitch = $derived(size === 'full' && atLeast(roleNow(), 'admin') && h?.role === 'primary' && stage === 'steady');
	let target = $state<string | null>(null);
	let askOpen = $state(false);
	let note = $state('');
	const flow = $derived(size === 'wall' && stage === 'steady');

	const compact = $derived(size === 'mini');
	const text = $derived(size === 'wall' ? 'text-[max(0.8rem,1vw)]' : compact ? 'text-xs' : 'text-[13px]');
	const pad = $derived(size === 'wall' ? 'p-2 lg:p-[0.7vw]' : compact ? 'p-2' : 'p-3');
	const tone = (lag: number) => (stage !== 'steady' ? 'text-bad opacity-20' : lag > 0 ? 'text-warn' : 'text-ok');
</script>

{#snippet headCard(kind: 'ok' | 'lost' | 'handover')}
	{@const lostK = kind === 'lost'}
	<div
		class="flex items-center gap-2.5 rounded-lg border {pad} {lostK ? 'border-bad bg-bad/10' : kind === 'handover' ? 'border-primary bg-primary/10' : 'border-line bg-surface'}"
	>
		<span class="relative shrink-0 {lostK ? 'text-bad' : 'text-primary'}">
			{#if kind === 'handover'}<ArrowRightLeft class="size-5" aria-hidden="true" />{:else}<Crown class="size-5" aria-hidden="true" />{/if}
			{#if lostK}<X class="text-bad absolute -inset-0.5 size-6" strokeWidth={3} aria-hidden="true" />{/if}
		</span>
		<span class="min-w-0 flex-1">
			<span class="flex min-w-0 items-baseline font-semibold tracking-tight {lostK ? 'text-bad' : kind === 'handover' ? 'text-primary' : ''}"><span class="shrink-0">{lostK ? 'Head lost' : kind === 'handover' ? 'Handing over' : 'HEAD'}</span>{#if kind === 'ok' && h?.head}<span class="text-muted-foreground ml-1.5 min-w-0 truncate font-normal" title="{headHost} {h.head.addr}">{headHost}</span>{/if}</span>
			<span class="text-muted-foreground num block {compact ? 'truncate' : ''}">
				{h?.role === 'standby' ? 'this console is on a standby' : `${h?.connected ?? 0}/${h?.membership ?? 0} PCs`}{#if kind === 'ok' && h?.head?.since_ms}{` · up ${duration(Date.now() - h.head.since_ms)}`}{/if}
			</span>
		</span>
		{#if h && !compact && h.role !== 'standby'}
			{#if hasQuorum(h.connected, h.membership)}
				<ShieldCheck class="text-ok ml-auto size-4 shrink-0" aria-label="Quorum: enough PCs are connected" />
			{:else}
				<ShieldAlert class="text-warn ml-auto size-4 shrink-0" aria-label="No quorum: waiting for more PCs" />
			{/if}
		{/if}
	</div>
{/snippet}

<div class="min-w-0 {text} {size === 'full' ? 'max-w-3xl' : ''}" role="group" aria-label="Head and standbys">
	{#if !h}
		{#if live.link === 'reconnecting'}
			<p class="text-muted-foreground">Cannot reach the head right now. This diagram returns when it does.</p>
		{:else}
			<div class="bg-raised animate-pulse rounded-lg {compact ? 'h-[120px]' : 'h-40'}" aria-busy="true"></div>
		{/if}
	{:else}
		<p class="text-muted-foreground mb-1.5 flex items-center gap-2">
			<span class="num text-foreground inline-block overflow-hidden font-semibold">
				{#key h.epoch}<span class="inline-block" in:fly={{ y: reduce ? 0 : 12, duration: reduce ? 0 : 350 }}>term {h.epoch}</span>{/key}
			</span>
			{#if stage === 'lost'}<span class="text-bad font-medium">no head, electing</span>{:else if stage === 'handover'}<span class="text-primary font-medium">planned switchover</span>{/if}
		</p>
		<div bind:this={topoEl} class="topo grid items-center gap-y-1.5">
			<div class="slot relative" style="--span:{Math.max(1, rows.length + slots || 1)}">
				<div bind:this={headEl} style={animating ? `transform:translate(${from.dx * glide.current}px,${from.dy * glide.current}px);position:relative;z-index:1` : undefined}>
					{@render headCard(stage === 'steady' ? 'ok' : stage)}
				</div>
				{#if animating}
					<div class="pointer-events-none absolute inset-0" style="opacity:{glide.current}" aria-hidden="true">{@render headCard('lost')}</div>
				{/if}
			</div>
			{#each rows as s, i (s.id)}
				<span class="ck-line {tone(s.lag)} {flow ? 'ck-flow' : ''}" aria-hidden="true"></span>
				<div data-first={i === 0 ? '' : undefined} class="flex min-w-0 items-center gap-2 rounded-lg border {pad} {stage === 'steady' && s.lag > 0 ? 'border-warn/60' : 'border-line'} bg-surface">
					<span class="bg-raised text-muted-foreground num grid size-5 shrink-0 place-items-center rounded-full text-[11px] font-semibold" title="Takeover order">{s.n}</span>
					{#if stage !== 'steady' && s.n === 1}<LoaderCircle class="text-warn size-4 shrink-0 motion-safe:animate-spin" aria-hidden="true" />{:else}<Eye class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />{/if}
					<span class="min-w-0 flex-1">
						<span class="block truncate font-medium" title={s.host}>{s.host}</span>
						<span class="num block leading-tight {stage !== 'steady' ? 'text-warn' : s.lag > 0 ? 'text-warn' : 'text-ok'}" title="{s.lab ?? ''} {standbyStatus(s, stage, !!joined[s.id])}">
							{!compact && s.lab ? `${s.lab} · ` : ''}{standbyStatus(s, stage, !!joined[s.id])}
						</span>
					</span>
					{#if canSwitch}
						<button type="button" class="{btnGhost} h-9 shrink-0 px-2.5 text-xs" onclick={() => { target = s.id; askOpen = true; }}>
							<ArrowRightLeft class="size-3.5" aria-hidden="true" />Make head
						</button>
					{/if}
				</div>
			{/each}
			{#if shown.length > rows.length}
				<p class="text-muted-foreground sm:col-start-3">+{shown.length - rows.length} more standbys</p>
			{/if}
			{#each Array(slots > 0 ? (compact ? 1 : slots) : 0) as _, i (i)}
				<span class="ck-line text-muted-foreground dashed {flow ? 'ck-flow' : ''}" aria-hidden="true"></span>
				<div class="border-line text-muted-foreground flex min-w-0 items-center gap-2 rounded-lg border border-dashed {pad}">
					<UserPlus class="size-4 shrink-0" aria-hidden="true" />
					<span class="min-w-0 flex-1 truncate" title={recruitWords(i === 0 ? (cand?.host ?? null) : null, live.haCfg?.selector ?? '')}>{recruitWords(i === 0 ? (cand?.host ?? null) : null, live.haCfg?.selector ?? '')}</span>
					{#if i === 0 && cand}<LoaderCircle class="size-4 shrink-0 motion-safe:animate-spin" aria-hidden="true" />{/if}
				</div>
			{/each}
			{#if rows.length === 0 && slots === 0}
				<span></span>
				<p class="text-muted-foreground border-line rounded-lg border border-dashed {pad}">
					{live.haCfg && live.haCfg.target_standbys === 0 ? 'No standby wanted (0 set). If the head fails, work stops until you start it again.' : 'No standby is connected. If the head fails, work stops until you start it again.'}
				</p>
			{/if}
		</div>
		{#if size !== 'mini' && pills.length}
			<ul class="mt-2 flex flex-wrap gap-1.5" aria-label="Labs">
				{#each pills as p (p.id)}
					<li class="bg-raised text-muted-foreground num flex max-w-full items-center gap-1.5 rounded-full px-2.5 py-1" title={p.noStandby ? `${pillWords(p)}: no standby in this lab` : pillWords(p)}>
						<span class="max-w-[14rem] truncate">{pillWords(p)}</span>
						{#if p.noStandby}<span class="bg-warn size-2 shrink-0 rounded-full" role="img" aria-label="no standby in this lab"></span>{/if}
					</li>
				{/each}
			</ul>
		{/if}
		{#if note}<p class="mt-2" role="status">{note}</p>{/if}
	{/if}
</div>

<MakeHeadDialog bind:open={askOpen} {target} onnote={(t) => (note = t)} />

<style>
	.topo {
		grid-template-columns: minmax(0, 1fr);
	}
	.ck-line {
		display: block;
		width: 2px;
		height: 0.9rem;
		margin-inline: auto;
		background: currentColor;
	}
	.ck-line.dashed {
		background: repeating-linear-gradient(180deg, currentColor 0 4px, transparent 4px 8px);
	}
	@media (min-width: 640px) {
		.topo {
			grid-template-columns: minmax(0, 1fr) 2.75rem minmax(0, 1.2fr);
			row-gap: 0.4rem;
		}
		.slot {
			grid-row: span var(--span);
		}
		.ck-line {
			width: 100%;
			height: 2px;
			margin: 0;
		}
		.ck-line.dashed {
			background: repeating-linear-gradient(90deg, currentColor 0 4px, transparent 4px 8px);
		}
	}
	@media (prefers-reduced-motion: no-preference) {
		.ck-flow {
			background: repeating-linear-gradient(180deg, currentColor 0 6px, transparent 6px 12px);
			background-size: 100% 12px;
			animation: ck-flow-y 0.8s linear infinite;
		}
		@keyframes ck-flow-y {
			to {
				background-position: 0 12px;
			}
		}
		@media (min-width: 640px) {
			.ck-flow {
				background: repeating-linear-gradient(90deg, currentColor 0 6px, transparent 6px 12px);
				background-size: 12px 100%;
				animation: ck-flow-x 0.8s linear infinite;
			}
			@keyframes ck-flow-x {
				to {
					background-position: 12px 0;
				}
			}
		}
	}
</style>
