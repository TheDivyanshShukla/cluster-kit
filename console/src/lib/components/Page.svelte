<script lang="ts" module>
	export interface PageCtx {
		active: string;
		/** tabs apply (phone, or every size when `always`) */
		tabbed: boolean;
		always: boolean;
	}
</script>

<script lang="ts">
	import { setContext, type Snippet } from 'svelte';
	import { followRouter, params, setQuery } from '#lib/urlstate.svelte.ts';

	// Page frame: title row, the phone tab strip (?tab=) and a grid that fills the rest of the viewport.
	// Panels inside choose their place with grid classes passed in `class` and their tab with `tab`.
	interface Props {
		title: string;
		subtitle?: string;
		tabs?: { id: string; label: string; badge?: string | number }[];
		/** show the strip (and one panel at a time) at every width, not only on phones */
		always?: boolean;
		class?: string;
		actions?: Snippet;
		children: Snippet;
	}
	let { title, subtitle, tabs, always = false, class: cls = '', actions, children }: Props = $props();
	followRouter();
	const active = $derived.by(() => {
		const t = params().get('tab');
		return tabs?.some((x) => x.id === t) ? t! : (tabs?.[0]?.id ?? '');
	});
	const ctx: PageCtx = {
		get active() {
			return active;
		},
		get tabbed() {
			return !!tabs;
		},
		get always() {
			return always;
		}
	};
	setContext('page', ctx);
	function go(e: KeyboardEvent, i: number) {
		const n = e.key === 'ArrowRight' ? i + 1 : e.key === 'ArrowLeft' ? i - 1 : null;
		if (n == null || !tabs) return;
		const t = tabs[(n + tabs.length) % tabs.length]!;
		setQuery('tab', t.id);
		e.preventDefault();
		queueMicrotask(() => document.getElementById(`tab-${t.id}`)?.focus());
	}
</script>

<svelte:head><title>{title} · Cluster Kit</title></svelte:head>

<div class="flex min-h-0 flex-1 flex-col gap-3">
	<header class="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2">
		<div class="min-w-0">
			<h1 class="text-lg leading-tight font-semibold tracking-tight">{title}</h1>
			{#if subtitle}<p class="text-muted-foreground max-w-3xl truncate text-[13px] max-md:hidden">{subtitle}</p>{/if}
		</div>
		{#if actions}<div class="ml-auto flex min-w-0 flex-wrap items-center gap-2">{@render actions()}</div>{/if}
	</header>
	{#if tabs && tabs.length > 1}
		<div role="tablist" aria-label="{title} sections" class="bg-raised border-line flex shrink-0 gap-0.5 self-start rounded-lg border p-0.5 max-sm:w-full {always ? '' : 'sm:hidden'}">
			{#each tabs as t, i (t.id)}
				<button
					type="button"
					role="tab"
					id="tab-{t.id}"
					aria-selected={active === t.id}
					tabindex={active === t.id ? 0 : -1}
					onclick={() => setQuery('tab', i === 0 ? null : t.id)}
					onkeydown={(e) => go(e, i)}
					class="min-h-9 rounded-md px-3 text-[13px] max-sm:flex-1 {active === t.id ? 'bg-surface text-foreground font-medium shadow-sm' : 'text-muted-foreground hover:text-foreground'}"
				>
					{t.label}{#if t.badge !== undefined}<span class="text-muted-foreground num ml-1.5 text-xs">{t.badge}</span>{/if}
				</button>
			{/each}
		</div>
	{/if}
	<div class="grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)] gap-3 {cls}">{@render children()}</div>
</div>
