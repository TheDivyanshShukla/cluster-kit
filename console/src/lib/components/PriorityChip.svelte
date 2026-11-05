<script lang="ts">
	import { ChevronDown, ChevronUp, ChevronsDown, ChevronsUp, Minus } from '@lucide/svelte';
	import { priorityLabel } from '#lib/org.ts';

	let { priority }: { priority: number } = $props();
	// icon + word, never colour alone
	const look = [
		{ icon: ChevronsUp, cls: 'text-bad bg-bad/10' },
		{ icon: ChevronUp, cls: 'text-warn bg-warn/10' },
		{ icon: Minus, cls: 'text-muted-foreground bg-raised' },
		{ icon: ChevronDown, cls: 'text-muted-foreground bg-raised' },
		{ icon: ChevronsDown, cls: 'text-muted-foreground bg-raised' }
	];
	const m = $derived(look[priority] ?? look[2]);
</script>

<span class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium {m.cls}" title="Priority">
	<m.icon class="size-3.5 shrink-0" strokeWidth={2} aria-hidden="true" />{priorityLabel(priority)}
</span>
