<script lang="ts">
	import { Crown, Eye, LoaderCircle, Minus, Plus, ShieldAlert, ShieldCheck } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { count } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import { btnPrimary, card } from '#lib/ui.ts';
	import { hasQuorum, lagWords, quorumWords, roleLabel } from '#lib/ha.ts';
	import { live, loadHaCfg } from '#lib/live.svelte.ts';

	// Overview shows the numbers and the standby list; Settings (`full`) adds the standby settings and the failover notes.
	// The Make head flow lives in HeadTopology, which sits above this card on Settings.
	let { full = false }: { full?: boolean } = $props();
	const h = $derived(live.ha);
	const meta = {
		primary: { icon: Crown, cls: 'text-ok bg-ok/10' },
		standby: { icon: Eye, cls: 'text-muted-foreground bg-raised' },
		candidate: { icon: LoaderCircle, cls: 'text-warn bg-warn/10' }
	} as const;
	// HA-10: how many standbys the head keeps, and which PCs may host one (admin)
	const canTune = $derived(full && atLeast(roleNow(), 'admin'));
	let want = $state(1);
	let sel = $state('');
	let matched = $state<string[] | null>(null);
	let tuned = $state(false);
	let tuneNote = $state('');
	$effect(() => {
		if (full)
			api.haSettings()
				.then((r) => ((want = r.settings.target_standbys), (sel = r.settings.selector), (tuned = true), (live.haCfg = r.settings)))
				.catch(() => (tuneNote = 'Could not load the standby settings.'));
	});
	const validWant = $derived(Number.isInteger(want) && want >= 0 && want <= 5);
	async function saveTune() {
		tuneNote = '';
		try {
			const r = await api.setHaSettings({ target_standbys: want, selector: sel.trim() });
			want = r.settings.target_standbys;
			sel = r.settings.selector;
			live.haCfg = r.settings;
			void loadHaCfg();
			tuneNote = 'Saved. The head recruits or releases standbys within a minute.';
		} catch (e) {
			tuneNote = e instanceof ApiError ? e.message : 'The head did not answer.';
		}
	}
	const hostOf = (id: string) => live.nodes[id]?.host ?? id;
</script>

<section class="{card} px-4 py-4" aria-label="High availability">
	<div class="flex flex-wrap items-center justify-between gap-2">
		<h2 class="font-medium">Head and standby</h2>
		{#if h}
			{@const m = meta[h.role]}
			<span class="inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium {m.cls}">
				<m.icon class="size-3.5 shrink-0 {h.role === 'candidate' ? 'motion-safe:animate-spin' : ''}" strokeWidth={2} aria-hidden="true" />{roleLabel(h.role)}
			</span>
		{/if}
	</div>
	{#if !h}
		{#if live.link === 'reconnecting'}
			<p class="text-muted-foreground mt-2">Cannot reach the head right now.</p>
		{:else}
			<div class="bg-raised mt-3 h-16 animate-pulse rounded-lg" aria-busy="true"></div>
		{/if}
	{:else}
		<dl class="mt-3 grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-4">
			<div><dt class="text-muted-foreground text-xs">Term</dt><dd class="num text-lg font-semibold">term {h.epoch}</dd></div>
			<div><dt class="text-muted-foreground text-xs">Dispatching</dt><dd class="text-lg font-semibold {h.dispatching ? 'text-ok' : 'text-warn'}">{h.dispatching ? 'On' : 'Off'}</dd></div>
			<div><dt class="text-muted-foreground text-xs">Latest change</dt><dd class="num text-lg font-semibold">{count(h.last_seq)}</dd></div>
			<div><dt class="text-muted-foreground text-xs">Safely stored</dt><dd class="num text-lg font-semibold">{count(h.durable_seq)}</dd></div>
		</dl>
		<p class="mt-3 flex items-start gap-2 text-[13px]">
			{#if hasQuorum(h.connected, h.membership)}
				<ShieldCheck class="text-ok mt-0.5 size-4 shrink-0" aria-hidden="true" />
			{:else}
				<ShieldAlert class="text-warn mt-0.5 size-4 shrink-0" aria-hidden="true" />
			{/if}
			<span class="num">{quorumWords(h.connected, h.membership)}</span>
		</p>
		{#if !full}
			<h3 class="mt-4 text-[13px] font-medium">Standby heads</h3>
			{#if h.standbys.length === 0}
				<p class="text-muted-foreground mt-1 text-[13px]">No standby is connected. If this head fails, the work stops until you start it again.</p>
			{:else}
				<ul class="divide-line mt-1 divide-y">
					{#each h.standbys as s (s.node_id)}
						<li class="flex flex-wrap items-baseline justify-between gap-x-3 py-2 text-[13px]">
							<span class="min-w-0 truncate font-mono text-xs" title={s.node_id}>{hostOf(s.node_id)}</span>
							<span class="num text-muted-foreground">at change {count(s.acked_seq)} · <span class={s.lag === 0 ? 'text-ok' : 'text-warn'}>{lagWords(s.lag)}</span></span>
						</li>
					{/each}
				</ul>
			{/if}
		{/if}
		{#if full && tuned}
			<h3 class="mt-4 text-[13px] font-medium">Standby heads to keep</h3>
			<p class="text-muted-foreground mt-1 text-[13px]">After a failover the head picks a new standby by itself, so this many are always ready.</p>
			<form class="mt-2 space-y-3" onsubmit={(e) => { e.preventDefault(); void saveTune(); }}>
				<fieldset disabled={!canTune} class="m-0 space-y-3 border-0 p-0">
					<div class="flex flex-wrap items-center gap-2 text-[13px]">
						<span id="ha-want">Keep</span>
						<div class="border-line bg-surface inline-flex items-center rounded-lg border" role="group" aria-labelledby="ha-want">
							<button type="button" class="hover:bg-raised grid size-9 place-items-center rounded-l-lg disabled:opacity-40" aria-label="One fewer standby" disabled={!canTune || want <= 0} onclick={() => (want = Math.max(0, want - 1))}><Minus class="size-4" aria-hidden="true" /></button>
							<span class="num border-line w-10 border-x text-center text-base font-semibold" aria-live="polite">{want}</span>
							<button type="button" class="hover:bg-raised grid size-9 place-items-center rounded-r-lg disabled:opacity-40" aria-label="One more standby" disabled={!canTune || want >= 5} onclick={() => (want = Math.min(5, want + 1))}><Plus class="size-4" aria-hidden="true" /></button>
						</div>
						<span>standby heads <span class="text-muted-foreground">(0 to 5)</span></span>
					</div>
					<SelectorBar bind:value={sel} bind:matched id="ha-sel" label="Which PCs may host a standby" emptyMeans="Every PC" matchWord="can be a standby" placeholder="!virtual" />
				</fieldset>
				{#if canTune}<button type="submit" class={btnPrimary} disabled={!validWant}>Save standby settings</button>
				{:else}<p class="text-muted-foreground text-[13px]">Changing this needs the admin role.</p>{/if}
				{#if tuneNote}<p class="text-[13px]" role="status">{tuneNote}</p>{/if}
			</form>
		{/if}
		{#if full}
			<h3 class="mt-4 text-[13px] font-medium">If the head fails</h3>
			<ul class="text-muted-foreground mt-1 list-disc space-y-1 pl-5 text-[13px]">
				<li>Each standby keeps a copy of every change. Results count as safe only once a standby has them.</li>
				<li>If the head goes silent for about 6 seconds, the most up-to-date standby takes over and the term goes up by one.</li>
				<li>PCs reconnect by themselves and resend any result they were not told was safe. Nothing is counted twice.</li>
				<li>The new head hands out work only once more than half of the PCs are back. Until then the wall shows "taking over".</li>
				<li>An old head that comes back sees the higher term and steps down to standby.</li>
				<li>To move the head on purpose (maintenance), use Make head on a standby above. Nothing is lost.</li>
			</ul>
		{/if}
	{/if}
</section>
