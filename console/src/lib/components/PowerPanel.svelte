<script lang="ts">
	import { Power } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { LabPower, PowerSettings } from '#lib/api/schema.ts';
	import Segmented from '#lib/components/Segmented.svelte';
	import { watts } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { powerCounts } from '#lib/topology.ts';
	import { btnPrimary, card, field } from '#lib/ui.ts';

	let { bare = false }: { bare?: boolean } = $props();
	let mode = $state<'off' | 'eco'>('off');
	let idle = $state(15);
	let keep = $state(4);
	let demand = $state(true);
	let loaded = $state(false);
	let busy = $state(false);
	let note = $state('');
	const canEdit = $derived(atLeast(roleNow(), 'operator'));
	const valid = $derived(Number.isInteger(idle) && idle >= 1 && idle <= 1440 && Number.isInteger(keep) && keep >= 0 && keep <= 10000);
	const totalW = $derived(live.labPower.reduce((a, l) => a + l.watts, 0));
	const c = $derived(powerCounts(Object.values(live.nodes)));
	const why = { off: 'Every PC stays on. Nothing is put to sleep unless an operator does it by hand.', eco: 'The head puts idle PCs to sleep and wakes them (Wake-on-LAN) when work needs them.' } as const;

	function fill(r: { power: PowerSettings; asleep: string[]; labs: LabPower[] }) {
		({ mode, idle_min: idle, keep_awake: keep, wake_on_demand: demand } = r.power);
		live.powerCfg = r.power;
		live.labPower = r.labs;
	}
	$effect(() => {
		api.power().then(fill).catch(() => (note = 'Could not load the power settings.')).finally(() => (loaded = true));
	});
	async function save() {
		busy = true;
		note = '';
		try {
			fill(await api.setPower({ mode, idle_min: idle, keep_awake: keep, wake_on_demand: demand }));
			note = 'Saved.';
		} catch (e) {
			note = e instanceof ApiError ? e.message : 'The head did not answer.';
		}
		busy = false;
	}
</script>

<section class={bare ? '' : `${card} px-4 py-4`} aria-label="Power">
	{#if !bare}<div class="flex flex-wrap items-center justify-between gap-2">
		<h2 class="flex items-center gap-2 font-medium"><Power class="text-muted-foreground size-4" aria-hidden="true" />Power</h2>
	</div>{/if}
	{#if !loaded}
		<div class="bg-raised h-40 animate-pulse rounded-lg" aria-busy="true"></div>
	{:else}
		<form class="space-y-3" onsubmit={(e) => { e.preventDefault(); void save(); }}>
			<fieldset disabled={!canEdit} class="m-0 space-y-3 border-0 p-0">
				<Segmented legend="Power mode" bind:value={mode} options={[{ value: 'off', label: 'Off' }, { value: 'eco', label: 'Eco' }]} />
				<p class="text-muted-foreground text-[13px]">{why[mode]}</p>
				<label class="flex flex-wrap items-center gap-x-2 gap-y-1 text-[13px]">Sleep a PC after it is idle for
					<input type="number" min="1" max="1440" bind:value={idle} class="{field} num w-20!" aria-label="Idle minutes before sleep" /> min
				</label>
				<label class="flex flex-wrap items-center gap-x-2 gap-y-1 text-[13px]">Keep at least
					<input type="number" min="0" max="10000" bind:value={keep} class="{field} num w-24!" aria-label="PCs to keep awake" /> PCs awake
				</label>
				<label class="flex min-h-9 items-center gap-2 text-[13px]"><input type="checkbox" role="switch" bind:checked={demand} class="size-4" />Wake PCs when work arrives</label>
			</fieldset>
			<p class="num text-muted-foreground text-[13px]">Now: {c.awake} awake, {c.asleep} asleep{c.waking ? `, ${c.waking} waking` : ''}. Eco never sleeps the head, standbys or busy PCs.</p>
			{#if live.labPower.length}
				<table class="w-full text-[13px]" aria-label="Power draw by lab">
					<thead><tr class="text-muted-foreground text-left text-xs"><th class="py-1 font-normal">Lab</th><th class="py-1 text-right font-normal">Draw</th><th class="py-1 text-right font-normal">Awake</th><th class="py-1 text-right font-normal">Asleep</th></tr></thead>
					<tbody class="divide-line divide-y">
						{#each live.labPower as l (l.lab)}
							<tr><td class="max-w-0 truncate py-1" title={l.lab}>{l.lab}</td><td class="num py-1 text-right">{watts(l.watts)}</td><td class="num py-1 text-right">{l.awake}</td><td class="num py-1 text-right">{l.asleep}{l.waking ? ` (+${l.waking})` : ''}</td></tr>
						{/each}
						<tr class="font-medium"><td class="py-1">All labs</td><td class="num py-1 text-right">{watts(totalW)}</td><td class="num py-1 text-right">{c.awake}</td><td class="num py-1 text-right">{c.asleep}</td></tr>
					</tbody>
				</table>
			{/if}
			<div class="flex flex-wrap items-center gap-3">
				{#if canEdit}<button type="submit" class={btnPrimary} disabled={busy || !valid}>{busy ? 'Saving…' : 'Save power settings'}</button>
				{:else}<p class="text-muted-foreground text-[13px]">Changing power settings needs the operator role.</p>{/if}
				{#if !valid}<p class="text-bad text-[13px]" role="alert">Idle minutes 1-1440; keep awake 0-10000.</p>{/if}
				{#if note}<p class="text-[13px]" role="status">{note}</p>{/if}
			</div>
		</form>
	{/if}
</section>
