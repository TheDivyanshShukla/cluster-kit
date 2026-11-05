<script lang="ts">
	import { Activity, ArrowRightLeft, Crown, ListChecks, Moon, Plug, Sunrise, UserPlus, User as UserIcon, WifiOff, X, Wrench } from '@lucide/svelte';
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
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { field } from '#lib/ui.ts';

	let filter = $state('');
	let replayAt = $state<number | null>(null);
	let pinned = $state<string | null>(null);
	let matched = $state<[number, number]>([0, 0]);
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
	}
	const node = $derived(pinned ? live.nodes[pinned] : undefined);
	const worker = $derived(live.cluster?.workers.find((w) => w.node_id === pinned));
	const recent = $derived(pinned ? all.filter((e) => idsIn(e, hosts).includes(pinned!)).slice(0, 6) : []);
</script>

<Page
	title="Live"
	subtitle="Every PC, the head and the work moving between them."
	tabs={[{ id: 'canvas', label: 'Canvas' }, { id: 'events', label: 'Events' }, { id: 'timeline', label: 'Timeline' }]}
	class="sm:grid-cols-[minmax(0,1fr)_17rem] sm:grid-rows-[minmax(0,1fr)_auto] lg:grid-cols-[minmax(0,1fr)_20rem]"
>
	{#snippet actions()}
		<label class="flex items-center gap-2">
			<span class="text-muted-foreground text-xs max-sm:sr-only">Filter <span class="max-lg:hidden">(lab=A job=12 state=lost)</span></span>
			<input class="{field} h-9 w-full font-mono sm:w-60" bind:value={filter} placeholder="lab=A state=lost" autocomplete="off" spellcheck="false" maxlength="200" aria-label="Filter PCs" />
		</label>
		{#if filter.trim()}<span class="text-muted-foreground num text-xs" role="status">{matched[0]} of {matched[1]} match</span>{/if}
	{/snippet}

	<Panel title="Canvas" tab="canvas" label="Live canvas" scroll={false} pad={false} class="sm:row-start-1">
		<div class="min-h-0 flex-1 p-1"><LiveCanvas bind:this={canvasRef} {filter} {replayAt} replayEvents={all} onpick={(id) => (pinned = id)} onmatch={(a, b) => (matched = [a, b])} /></div>
	</Panel>

	<Panel title="Timeline" tab="timeline" scroll={false} class="sm:row-start-2">
		<TimelineScrubber events={all} bind:at={replayAt} />
	</Panel>

	<div class="contents sm:row-span-2 sm:col-start-2 sm:row-start-1 sm:flex sm:min-h-0 sm:flex-col sm:gap-3">
		<Panel title="Events" tab="events" label="Event rail" class="sm:min-h-0 sm:flex-1" pad={false} count={all.length}>
			<ul class="divide-line divide-y">
				{#each all.slice(0, 150) as e (e.id)}
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
		</Panel>
		{#if node}
			<Panel title={node.host} tab="canvas" label="Selected PC" scroll={false} class="sm:max-h-[55%] sm:shrink-0">
				{#snippet actions()}
					<NodeStateChip state={node.state} online={node.online} power={node.power} id={node.node_id} />
					<button type="button" class="text-muted-foreground hover:text-foreground inline-flex size-8 items-center justify-center" aria-label="Close" onclick={() => (pinned = null)}><X class="size-4" aria-hidden="true" /></button>
				{/snippet}
				<div class="thin max-h-[40dvh] min-h-0 overflow-auto">
					<p class="text-muted-foreground text-xs">{live.labs.find((l) => l.id === node.lab_id)?.name ?? 'no lab'}{worker?.rate ? ` · ${rate(worker.rate)}` : ''}</p>
					{#if recent.length}
						<ul class="text-muted-foreground mt-2 space-y-0.5 text-xs">{#each recent as e (e.id)}<li class="truncate"><span class="num">{stamp(e.ts_ms)}</span> {railText(e)}</li>{/each}</ul>
					{:else}<p class="text-muted-foreground mt-2 text-xs">No recent events for this PC.</p>{/if}
					{#if replayAt == null}<div class="mt-3"><NodeActions ids={[node.node_id]} ondone={() => (pinned = null)} /></div>{/if}
				</div>
			</Panel>
		{/if}
	</div>
</Page>
