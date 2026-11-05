<script lang="ts">
	import { getContext, type Snippet } from 'svelte';
	import ScrollFade from '#lib/components/ScrollFade.svelte';
	import type { PageCtx } from '#lib/components/Page.svelte';

	// A dashboard panel: fixed header (title, count, actions), optional toolbar, and a body that scrolls on its own.
	// `scroll={false}` hands the body to a child that scrolls itself (DataTable) or that scales to fit (canvas, topology).
	interface Props {
		title: string;
		/** which phone tab shows this panel (default: its title) */
		tab?: string;
		count?: string | number;
		scroll?: boolean;
		pad?: boolean;
		class?: string;
		label?: string;
		actions?: Snippet;
		toolbar?: Snippet;
		footer?: Snippet;
		children: Snippet;
	}
	let { title, tab = title.toLowerCase(), count, scroll = true, pad = true, class: cls = '', label, actions, toolbar, footer, children }: Props = $props();
	const ctx = getContext<PageCtx | undefined>('page');
	const off = $derived(ctx && ctx.tabbed && ctx.active !== tab);
</script>

<section
	class="bg-surface border-line flex min-h-0 min-w-0 flex-col overflow-hidden rounded-lg border {off ? (ctx?.always ? 'hidden' : 'max-sm:hidden') : ''} {cls}"
	aria-label={label ?? title}
>
	<header class="border-line flex min-h-10 shrink-0 items-center gap-2 border-b px-3 py-1.5 {ctx?.tabbed && !ctx.always && !actions ? 'max-sm:hidden' : ''}">
		<h2 class="truncate text-[13px] font-semibold">{title}</h2>
		{#if count !== undefined}<span class="bg-raised text-muted-foreground num shrink-0 rounded-full px-2 py-0.5 text-xs">{count}</span>{/if}
		{#if actions}<div class="ml-auto flex min-w-0 items-center gap-1.5">{@render actions()}</div>{/if}
	</header>
	{#if toolbar}<div class="border-line shrink-0 border-b px-3 py-2">{@render toolbar()}</div>{/if}
	{#if scroll}
		<ScrollFade><div class={pad ? 'p-3' : ''}>{@render children()}</div></ScrollFade>
	{:else}
		<div class="flex min-h-0 flex-1 flex-col {pad ? 'p-3' : ''}">{@render children()}</div>
	{/if}
	{#if footer}<div class="border-line shrink-0 border-t px-3 py-2">{@render footer()}</div>{/if}
</section>
