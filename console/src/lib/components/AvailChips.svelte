<script lang="ts">
	import { CalendarOff, Gauge, User } from '@lucide/svelte';

	let { presence, blocked, cap }: { presence: 'busy' | 'idle'; blocked: boolean; cap: number | null } = $props();
</script>

<span class="flex flex-wrap gap-1">
	{#if presence === 'busy'}<span class="text-busy bg-busy/10 inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium whitespace-nowrap"><User class="size-3.5" aria-hidden="true" />In use</span>{/if}
	{#if blocked}<span class="text-bad bg-bad/10 inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium whitespace-nowrap"><CalendarOff class="size-3.5" aria-hidden="true" />Blocked by timetable</span>{/if}
	{#if cap != null}<span class="text-warn bg-warn/10 inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium whitespace-nowrap"><Gauge class="size-3.5" aria-hidden="true" />Cap {cap} thread{cap === 1 ? '' : 's'}</span>{/if}
	{#if presence !== 'busy' && !blocked && cap == null}<span class="text-muted-foreground">—</span>{/if}
</span>
