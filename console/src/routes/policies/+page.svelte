<script lang="ts">
	import BookingsPanel from '#lib/components/BookingsPanel.svelte';
	import TimetablePanel from '#lib/components/TimetablePanel.svelte';
	import CapsPanel from '#lib/components/CapsPanel.svelte';
	import ConfigPanel from '#lib/components/ConfigPanel.svelte';
	import PoolsPanel from '#lib/components/PoolsPanel.svelte';
	import ReservesPanel from '#lib/components/ReservesPanel.svelte';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';

	const canEdit = $derived(atLeast(roleNow(), 'operator'));
</script>

<svelte:head><title>Policies · Cluster Kit</title></svelte:head>

<h1 class="text-xl font-semibold tracking-tight">Policies</h1>
<p class="text-muted-foreground mt-1">Timetable, bookings, pools, thread caps and reserves decide which PCs jobs may use.</p>

<div class="mt-6 grid max-w-5xl gap-8">
	<TimetablePanel {canEdit} />
	<BookingsPanel {canEdit} />
	<ReservesPanel {canEdit} />
	<CapsPanel {canEdit} />
	<PoolsPanel {canEdit} />
	{#if atLeast(roleNow(), 'admin')}<ConfigPanel />{/if}
</div>
