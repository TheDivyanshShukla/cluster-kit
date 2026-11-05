<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { Gauge } from '@lucide/svelte';
	interface Props {
		icon: typeof Gauge; // any lucide icon component
		title: string;
		body?: string;
		action?: Snippet;
	}
	let { icon: Icon, title, body, action }: Props = $props();
</script>

<div class="border-line flex flex-col items-center rounded-lg border border-dashed px-8 py-14 text-center">
	<Icon class="text-muted-foreground mb-4 size-10 shrink-0" strokeWidth={1.5} aria-hidden="true" />
	<h2 class="text-[15px] font-semibold">{title}</h2>
	{#if body}<p class="text-muted-foreground mt-1 max-w-sm text-balance">{body}</p>{/if}
	{#if action}<div class="mt-5">{@render action()}</div>{/if}
</div>
