<script lang="ts">
	import { Trash2 } from '@lucide/svelte';
	import type { LabRow } from '#lib/api/schema.ts';
	import { btnGhost, field } from '#lib/ui.ts';

	interface Props {
		lab: LabRow;
		busy: boolean;
		onsave: (v: { name: string; w: number; h: number }) => void;
		ondelete: () => void;
	}
	let { lab, busy, onsave, ondelete }: Props = $props();
	// svelte-ignore state_referenced_locally
	let v = $state({ name: lab.name, w: lab.w, h: lab.h }); // editable copy; Save sends it
	const same = $derived(v.name === lab.name && v.w === lab.w && v.h === lab.h);
	const dim = `num ${field} mt-0.5 block w-16 md:w-16`;
</script>

<form class="flex flex-wrap items-end gap-2" onsubmit={(e) => { e.preventDefault(); onsave({ name: v.name.trim(), w: v.w, h: v.h }); }}>
	<label class="min-w-32 flex-1 text-xs"><span class="text-muted-foreground">Name</span><input required maxlength="60" bind:value={v.name} class="{field} mt-0.5" /></label>
	<label class="text-xs"><span class="text-muted-foreground">Columns</span><input type="number" min="1" max="64" required bind:value={v.w} class={dim} /></label>
	<label class="text-xs"><span class="text-muted-foreground">Rows</span><input type="number" min="1" max="64" required bind:value={v.h} class={dim} /></label>
	<button type="submit" class={btnGhost} disabled={busy || same}>Save</button>
	<button type="button" class="{btnGhost} text-bad" disabled={busy} aria-label="Delete {lab.name}" onclick={ondelete}><Trash2 class="size-4" aria-hidden="true" /></button>
</form>
