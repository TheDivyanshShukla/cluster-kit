<script lang="ts" generics="T extends string">
	// A custom select: button + listbox with arrow keys, Enter/Space, Escape, type-to-close on outside click.
	import { Check, ChevronDown } from '@lucide/svelte';
	import { field } from '#lib/ui.ts';

	interface Props {
		label: string;
		options: readonly { value: T; label: string; hint?: string }[];
		value: T;
		id?: string;
	}
	let { label, options, value = $bindable(), id = `lb-${Math.random().toString(36).slice(2, 8)}` }: Props = $props();
	let open = $state(false);
	let active = $state(0);
	let root: HTMLDivElement;
	const cur = $derived(options.find((o) => o.value === value) ?? options[0]);

	function show() {
		active = Math.max(0, options.findIndex((o) => o.value === value));
		open = true;
	}
	function pick(i: number) {
		value = options[i].value;
		open = false;
		root.querySelector<HTMLButtonElement>('button')?.focus();
	}
	function key(e: KeyboardEvent) {
		if (!open) {
			if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(e.key)) (e.preventDefault(), show());
			return;
		}
		if (e.key === 'ArrowDown') (e.preventDefault(), (active = (active + 1) % options.length));
		else if (e.key === 'ArrowUp') (e.preventDefault(), (active = (active - 1 + options.length) % options.length));
		else if (e.key === 'Home') (e.preventDefault(), (active = 0));
		else if (e.key === 'End') (e.preventDefault(), (active = options.length - 1));
		else if (e.key === 'Enter' || e.key === ' ') (e.preventDefault(), pick(active));
		else if (e.key === 'Escape' || e.key === 'Tab') open = false;
	}
</script>

<svelte:window onpointerdown={(e) => open && !root.contains(e.target as Node) && (open = false)} />

<div class="relative" bind:this={root}>
	<span class="mb-1 block text-xs font-medium" id="{id}-l">{label}</span>
	<button
		type="button"
		class="{field} flex items-center justify-between gap-2 text-left"
		role="combobox"
		aria-haspopup="listbox"
		aria-expanded={open}
		aria-controls="{id}-list"
		aria-labelledby="{id}-l {id}-b"
		id="{id}-b"
		onclick={() => (open ? (open = false) : show())}
		onkeydown={key}
	>
		<span class="truncate">{cur.label}</span><ChevronDown class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />
	</button>
	{#if open}
		<ul id="{id}-list" role="listbox" aria-labelledby="{id}-l" class="bg-surface border-line absolute z-20 mt-1 max-h-72 w-full min-w-56 overflow-auto rounded-lg border p-1 shadow-lg">
			{#each options as o, i (o.value)}
				<li
					role="option"
					aria-selected={o.value === value}
					class="flex cursor-pointer items-start gap-2 rounded-md px-2.5 py-1.5 text-[13px] {i === active ? 'bg-raised' : ''}"
					onpointerenter={() => (active = i)}
					onclick={() => pick(i)}
					onkeydown={() => {}}
				>
					<Check class="mt-0.5 size-4 shrink-0 {o.value === value ? '' : 'invisible'}" aria-hidden="true" />
					<span class="min-w-0"><span class="block font-medium">{o.label}</span>{#if o.hint}<span class="text-muted-foreground block text-xs">{o.hint}</span>{/if}</span>
				</li>
			{/each}
		</ul>
	{/if}
</div>
