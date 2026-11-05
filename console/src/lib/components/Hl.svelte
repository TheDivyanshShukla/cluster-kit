<script lang="ts">
	import { splitMatches } from '#lib/listing.ts';
	// Text with the search words marked.
	let { text, q = '' }: { text: string | null | undefined; q?: string } = $props();
	const parts = $derived(splitMatches(text ?? '', q));
</script>

{#if q.trim()}{#each parts as p, i (i)}{#if p.hit}<mark class="bg-warn/30 rounded-sm text-inherit">{p.t}</mark>{:else}{p.t}{/if}{/each}{:else}{text}{/if}
