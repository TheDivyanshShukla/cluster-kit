<script lang="ts">
	import { Activity, ArrowRightLeft, Crown, ListChecks, Moon, Plug, Sunrise, UserPlus, User as UserIcon, WifiOff, X, Wrench, ChevronUp } from '@lucide/svelte';
	import type { Component } from 'svelte';
	import { api } from '#lib/api/client.ts';
	import type { EventRow } from '#lib/api/schema.ts';
	import { hostIndex, idsIn, railKind, railText, type RailKey, type Tone } from '#lib/canvas/effects.ts';
	import { CLS_WORDS } from '#lib/canvas/state.ts';
	import LiveCanvas from '#lib/components/LiveCanvas.svelte';
	import NodeActions from '#lib/components/NodeActions.svelte';
	import NodeStateChip from '#lib/components/NodeStateChip.svelte';
	import TimelineScrubber from '#lib/components/TimelineScrubber.svelte';
	import { rate } from '#lib/format.ts';
	import { mergeRows } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { card, field } from '#lib/ui.ts';

	let filter = $state('');
	let replayAt = $state<number | null>(null);
	let pinned = $state<string | null>(null);
	let matched = $state<[number, number]>([0, 0]);
	let sheet = $state(false);
	let canvasRef = $state<LiveCanvas>();

	// history for the rail and the replay: the last 30 minutes, newest first
	let fetched = $state<EventRow[]>([]);
	$effect(() => {
		(async () => {
			let before: number | null = null;
			const rows: EventRow[] = [];
			for (let page = 0; page < 5; page++) {
				const r = await api.events(200, before);
				rows.push(...r.events);
				before = r.next;
				if (before == null || (r.events.at(-1)?.ts_ms ?? 0) < Date.now() - 31 * 60_000) break;
			}
			fetched = rows;
		})().catch(() => {});
	});
	const all = $derived(mergeRows(fetched, live.events));
	const hosts = $derived(hostIndex(Object.values(live.nodes)));

	const icons: Record<RailKey, Component> = { lost: WifiOff, join: Plug, sleep: Moon, wake: Sunrise, head: Crown, recruit: UserPlus, switch: ArrowRightLeft, job: ListChecks, state: Wrench, user: UserIcon, other: Activity };
	const toneCls: Record<Tone, string> = { ok: 'text-ok', bad: 'text-bad', warn: 'text-warn', busy: 'text-primary', idle: 'text-muted-foreground' };
	const stamp = (t: number) => new Date(t).toLocaleTimeString('en-GB');

	function pick(ev: EventRow) {
		const ids = idsIn(ev, hosts);
		canvasRef?.flash(ids);
		if (ids[0]) pinned = ids[0];
		sheet = false;
	}
	const node = $derived(pinned ? live.nodes[pinned] : undefined);
	const worker = $derived(live.cluster?.workers.find((w) => w.node_id === pinned));
	const recent = $derived(pinned ? all.filter((e) => idsIn(e, hosts).includes(pinned!)).slice(0, 6) : []);
</script>

<svelte:head><title>Live · Cluster Kit</title></svelte:head>

<div class="flex flex-wrap items-end justify-between gap-3">
	<div>
		<h1 class="text-xl font-semibold tracking-tight">Live</h1>
		<p class="text-muted-foreground mt-1">Every PC, the head and the work moving between them.</p>
	</div>
	<label class="w-full sm:w-72">
		<span class="mb-1 block text-xs font-medium">Filter <span class="text-muted-foreground font-normal">(lab=A job=12 state=lost)</span></span>
		<input class="{field} font-mono" bind:value={filter} placeholder="lab=A state=lost" autocomplete="off" spellcheck="false" maxlength="200" />
	</label>
</div>
{#if filter.trim()}<p class="text-muted-foreground num mt-1 text-xs" role="status">{matched[0]} of {matched[1]} PCs match; the rest are dimmed.</p>{/if}

<div class="mt-4 grid min-w-0 gap-3 md:grid-cols-[minmax(0,1fr)_320px]">
	<div class="flex min-w-0 flex-col gap-3">
		<div class="{card} h-[min(62dvh,620px)] min-h-[320px] p-1">
			<LiveCanvas bind:this={canvasRef} {filter} {replayAt} replayEvents={all} onpick={(id) => (pinned = id)} onmatch={(a, b) => (matched = [a, b])} />
		</div>
		<TimelineScrubber events={all} bind:at={replayAt} />
		{#if node}
			<section class="{card} p-3" aria-label="Selected PC">
				<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
					<h2 class="font-semibold">{node.host}</h2>
					<NodeStateChip state={node.state} online={node.online} power={node.power} id={node.node_id} />
					<span class="text-muted-foreground text-xs">{live.labs.find((l) => l.id === node.lab_id)?.name ?? 'no lab'}{worker?.rate ? ` · ${rate(worker.rate)}` : ''}</span>
					<button type="button" class="text-muted-foreground hover:text-foreground ml-auto inline-flex h-9 items-center gap-1 px-1 text-xs" onclick={() => (pinned = null)}><X class="size-4" aria-hidden="true" />Close</button>
				</div>
				{#if recent.length}
					<ul class="text-muted-foreground mt-2 space-y-0.5 text-xs">{#each recent as e (e.id)}<li class="truncate"><span class="num">{stamp(e.ts_ms)}</span> {railText(e)}</li>{/each}</ul>
				{:else}<p class="text-muted-foreground mt-2 text-xs">No recent events for this PC.</p>{/if}
				{#if replayAt == null}<div class="mt-3"><NodeActions ids={[node.node_id]} ondone={() => (pinned = null)} /></div>{/if}
			</section>
		{/if}
	</div>

	<!-- event rail: a column on md+, a bottom sheet on phones -->
	<aside class="max-md:fixed max-md:inset-x-0 max-md:bottom-0 max-md:z-30 md:h-[calc(min(62dvh,620px)+7.5rem)] md:min-h-[420px]" aria-label="Events">
		<div class="{card} flex h-full min-h-0 flex-col max-md:rounded-b-none max-md:shadow-lg">
			<button type="button" class="flex h-11 shrink-0 items-center gap-2 px-3 text-left text-[13px] font-medium md:cursor-default" aria-expanded={sheet} onclick={() => (sheet = !sheet)}>
				Events
				<span class="text-muted-foreground min-w-0 flex-1 truncate font-normal md:hidden">{all[0] ? railText(all[0]) : ''}</span>
				<ChevronUp class="ml-auto size-4 transition-transform md:hidden {sheet ? 'rotate-180' : ''}" aria-hidden="true" />
			</button>
			<ul class="border-line min-h-0 flex-1 overflow-y-auto border-t {sheet ? 'max-md:max-h-[55dvh]' : 'max-md:hidden'}">
				{#each all.slice(0, 100) as e (e.id)}
					{@const k = railKind(e.kind)}
					{@const Icon = icons[k.key]}
					<li>
						<button type="button" class="hover:bg-raised flex min-h-11 w-full items-start gap-2 px-3 py-2 text-left text-[13px]" onclick={() => pick(e)}>
							<Icon class="mt-0.5 size-4 shrink-0 {toneCls[k.tone]}" aria-hidden="true" />
							<span class="min-w-0 flex-1 break-words">{railText(e)}</span>
							<span class="text-muted-foreground num shrink-0 text-xs">{stamp(e.ts_ms)}</span>
						</button>
					</li>
				{:else}
					<li class="text-muted-foreground px-3 py-4 text-[13px]">Nothing has happened yet. Joins, losses, sleep and head changes appear here.</li>
				{/each}
			</ul>
		</div>
	</aside>
</div>
<div class="h-12 md:hidden" aria-hidden="true"></div>
