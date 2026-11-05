<script lang="ts">
	import { Plus } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { LabRow } from '#lib/api/schema.ts';
	import LabEditRow from '#lib/components/LabEditRow.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import { live, loadFleet } from '#lib/live.svelte.ts';
	import { btnGhost, btnPrimary, field } from '#lib/ui.ts';

	let error = $state('');
	let busy = $state(false);
	let nw = $state({ name: '', w: 8, h: 5 });
	let del = $state<LabRow | null>(null);
	let delOpen = $state(false);

	async function wrap(fn: () => Promise<unknown>) {
		busy = true;
		error = '';
		try {
			await fn();
			await loadFleet();
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'That did not work.';
		}
		busy = false;
	}
	const save = (l: LabRow, v: { name: string; w: number; h: number }) => wrap(() => api.patchLab(l.id, v));
	const create = (e: SubmitEvent) => {
		e.preventDefault();
		return wrap(async () => {
			await api.createLab(nw.name.trim(), nw.w, nw.h);
			nw = { name: '', w: 8, h: 5 };
		});
	};
	const dim = 'num ' + field + ' w-16 md:w-16';
</script>

<div class="grid gap-3">
	<ul class="grid gap-2">
		{#each live.labs as l (l.id)}
			<li><LabEditRow lab={l} {busy} onsave={(v) => save(l, v)} ondelete={() => { del = l; delOpen = true; }} /></li>
		{/each}
	</ul>
	<form class="border-line flex flex-wrap items-end gap-2 border-t pt-3" onsubmit={create}>
		<label class="min-w-32 flex-1 text-xs"><span class="text-muted-foreground">New lab name</span><input required maxlength="60" bind:value={nw.name} placeholder="Lab A" class="{field} mt-0.5" /></label>
		<label class="text-xs"><span class="text-muted-foreground">Columns</span><input type="number" min="1" max="64" required bind:value={nw.w} class="{dim} mt-0.5 block" /></label>
		<label class="text-xs"><span class="text-muted-foreground">Rows</span><input type="number" min="1" max="64" required bind:value={nw.h} class="{dim} mt-0.5 block" /></label>
		<button type="submit" class={btnPrimary} disabled={busy}><Plus class="size-4" aria-hidden="true" />Create lab</button>
	</form>
	{#if error}<p class="text-bad text-[13px]" role="alert">{error}</p>{/if}
</div>

<ConfirmDialog
	bind:open={delOpen}
	title="Delete {del?.name}?"
	body="Its PCs go back to the unplaced list. This cannot be undone."
	confirm="Delete lab"
	{busy}
	onconfirm={() => del && wrap(async () => { await api.deleteLab(del!.id); delOpen = false; })}
/>
