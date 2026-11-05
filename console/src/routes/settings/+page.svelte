<script lang="ts">
	import HaPanel from '#lib/components/HaPanel.svelte';
	import HeadTopology from '#lib/components/HeadTopology.svelte';
	import PowerPanel from '#lib/components/PowerPanel.svelte';
	import { card } from '#lib/ui.ts';
</script>

<svelte:head><title>Settings · Cluster Kit</title></svelte:head>

<h1 class="text-xl font-semibold tracking-tight">Settings</h1>
<p class="text-muted-foreground mt-1">Power saving, and how the head keeps working when a machine fails.</p>
<section class="{card} mt-6 px-4 py-4" aria-label="Who holds the cluster">
	<h2 class="mb-3 font-medium">Head and standbys</h2>
	<HeadTopology size="full" />
</section>
<div class="mt-4 grid items-start gap-4 lg:grid-cols-2"><PowerPanel /><HaPanel full /></div>
