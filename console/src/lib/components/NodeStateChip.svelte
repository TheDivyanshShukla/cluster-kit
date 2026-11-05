<script lang="ts">
	import { Archive, Ban, CircleCheck, CircleHelp, Wrench, ArrowDownToLine, WifiOff, Moon, Sunrise, TriangleAlert } from '@lucide/svelte';
	import type { NodeState } from '#lib/api/schema.ts';
	import { live } from '#lib/live.svelte.ts';
	import { wakeView } from '#lib/topology.ts';

	let { state: nodeState, online = true, power = 'awake', id = '' }: { state: NodeState; online?: boolean; power?: 'awake' | 'asleep' | 'waking'; id?: string } = $props();
	// Seconds since the magic packet, counted from when this console first saw the PC waking.
	let now = $state(Date.now());
	$effect(() => {
		if (power !== 'waking') return;
		const t = setInterval(() => (now = Date.now()), 1000);
		return () => clearInterval(t);
	});
	const wake = $derived(power === 'waking' ? wakeView(Math.max(0, Math.floor((now - (live.wakeSince[id] ?? now)) / 1000))) : null);
	// Each state has its own icon and word; colour only reinforces.
	const meta = {
		pending: { icon: CircleHelp, label: 'Pending', cls: 'text-warn bg-warn/10' },
		active: { icon: CircleCheck, label: 'Active', cls: 'text-ok bg-ok/10' },
		cordoned: { icon: Ban, label: 'Cordoned', cls: 'text-muted-foreground bg-raised' },
		draining: { icon: ArrowDownToLine, label: 'Draining', cls: 'text-busy bg-busy/10' },
		maintenance: { icon: Wrench, label: 'Maintenance', cls: 'text-bad bg-bad/10' },
		retired: { icon: Archive, label: 'Retired', cls: 'text-muted-foreground bg-raised' }
	} as const;
	const m = $derived(meta[nodeState]);
</script>

<span class="inline-flex flex-wrap items-center gap-1.5">
	<span class="inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium {m.cls}">
		<m.icon class="size-3.5 shrink-0" strokeWidth={2} aria-hidden="true" />{m.label}
	</span>
	{#if power === 'asleep'}<span class="text-muted-foreground bg-raised inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium"><Moon class="size-3.5" aria-hidden="true" />Asleep</span>
	{:else if wake}
		<span class="num inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium {wake.failed ? 'text-bad bg-bad/10' : 'text-warn bg-warn/10'}" title={wake.hint}>
			{#if wake.failed}<TriangleAlert class="size-3.5" aria-hidden="true" />{:else}<Sunrise class="size-3.5" aria-hidden="true" />{/if}{wake.label}
		</span>
		{#if wake.failed}<span class="text-muted-foreground text-xs">{wake.hint}</span>{/if}
	{:else if !online}<span class="text-muted-foreground inline-flex items-center gap-1 text-xs"><WifiOff class="size-3.5" aria-hidden="true" />offline</span>{/if}
</span>
