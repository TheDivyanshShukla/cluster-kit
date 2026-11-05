<script lang="ts">
	import CandidatesTable from '#lib/components/CandidatesTable.svelte';
	import HaPanel from '#lib/components/HaPanel.svelte';
	import HeadTopology from '#lib/components/HeadTopology.svelte';
	import PowerPanel from '#lib/components/PowerPanel.svelte';
	import TunablesPanel from '#lib/components/TunablesPanel.svelte';
	import { card } from '#lib/ui.ts';
</script>

<svelte:head><title>System · Cluster Kit</title></svelte:head>

<h1 class="text-xl font-semibold tracking-tight">System</h1>
<p class="text-muted-foreground mt-1">Everything adjustable in one place: the head and its standbys, power saving, and every timing and limit.</p>
<nav class="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-[13px]" aria-label="On this page">
	<a href="#head" class="text-primary hover:underline">Head and standbys</a><a href="#power" class="text-primary hover:underline">Power</a><a href="#all" class="text-primary hover:underline">All settings</a>
</nav>

<div class="mt-5 max-w-5xl space-y-4">
	<h2 id="head" class="scroll-mt-4 text-base font-semibold">Head and standbys</h2>
	<section class="{card} px-4 py-4" aria-label="Who holds the cluster">
		<HeadTopology size="full" />
		<div class="border-line mt-5 border-t pt-4"><CandidatesTable /></div>
	</section>
	<HaPanel full />
	<h2 id="power" class="scroll-mt-4 pt-2 text-base font-semibold">Power</h2>
	<PowerPanel />
	<h2 id="all" class="scroll-mt-4 pt-2 text-base font-semibold">All settings</h2>
	<TunablesPanel />
</div>
