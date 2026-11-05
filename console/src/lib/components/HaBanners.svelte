<script lang="ts">
	import { Eye } from '@lucide/svelte';
	import { live } from '#lib/live.svelte.ts';

	// Full-width notice shown on every page when this console is served by a standby head.
	const primary = $derived(live.standby503?.primary ?? null);
	const href = $derived(primary ? (primary.startsWith('http') ? primary : `http://${primary}`) : null);
</script>

<p class="bg-raised border-line mb-4 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-lg border px-3 py-2 text-[13px]" role="status">
	<Eye class="size-4 shrink-0" aria-hidden="true" />
	<span>This is the standby head.{#if primary} The primary is <a {href} class="text-primary font-medium hover:underline">{primary}</a>.{:else} Open the primary head to make changes.{/if} This page is read-only.</span>
</p>
