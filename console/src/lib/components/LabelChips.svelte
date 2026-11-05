<script lang="ts">
	import { X } from '@lucide/svelte';

	interface Props {
		labels: Record<string, string>;
		/** given: each chip gets a remove button */
		onremove?: (key: string) => void;
		disabled?: boolean;
	}
	let { labels, onremove, disabled = false }: Props = $props();
	const entries = $derived(Object.entries(labels).sort(([a], [b]) => a.localeCompare(b)));
</script>

{#if entries.length}
	<ul class="flex flex-wrap gap-1">
		{#each entries as [k, v] (k)}
			<li class="bg-raised border-line inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono text-[11px]">
				<span class="max-w-32 truncate" title="{k}{v ? `=${v}` : ''}">{k}{v ? `=${v}` : ''}</span>
				{#if onremove}
					<button type="button" class="text-muted-foreground hover:text-bad -mr-0.5 rounded disabled:opacity-50" {disabled} aria-label="Remove label {k}" onclick={() => onremove(k)}>
						<X class="size-3" aria-hidden="true" />
					</button>
				{/if}
			</li>
		{/each}
	</ul>
{:else}
	<span class="text-muted-foreground">—</span>
{/if}
