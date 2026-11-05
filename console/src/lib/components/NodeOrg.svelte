<script lang="ts">
	import { Layers, Tag } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { live, loadFleet } from '#lib/live.svelte.ts';
	import { parseLabel } from '#lib/org.ts';
	import { btnGhost, btnPrimary, field } from '#lib/ui.ts';

	// Bulk label and pool editing for the selected PCs (operator and up; the parent only renders it for them).
	let { ids }: { ids: string[] } = $props();
	let poolNames = $state<string[]>([]);
	$effect(() => void api.pools().then((r) => (poolNames = r.pools.map((p) => p.name))).catch(() => {}));
	let text = $state('');
	let pool = $state('');
	let busy = $state(false);
	let error = $state('');
	let note = $state('');
	const keys = $derived([...new Set(ids.flatMap((i) => Object.keys(live.nodes[i]?.labels ?? {})))].sort());

	async function labelAll(remove: boolean) {
		const p = parseLabel(text);
		if ('error' in p) return void (error = p.error);
		busy = true;
		error = note = '';
		try {
			for (const id of ids) live.nodes[id] = (await api.setLabels(id, { [p.key]: remove ? null : p.value })).node;
			note = remove ? `Removed ${p.key} from ${ids.length} PC${ids.length === 1 ? '' : 's'}.` : `Labelled ${ids.length} PC${ids.length === 1 ? '' : 's'}.`;
			text = '';
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not change the labels.';
		}
		busy = false;
	}
	async function savePool() {
		const name = pool.trim();
		if (!name) return void (error = 'Give the pool a name.');
		busy = true;
		error = note = '';
		try {
			// an existing pool keeps its members: the selection is added to them
			const old = (await api.pools()).pools.find((p) => p.name === name);
			await api.putPool(name, [...new Set([...(old?.members ?? []), ...ids])], old?.note ?? '');
			note = `${old ? 'Added to' : 'Created'} pool ${name}.`;
			pool = '';
			void loadFleet();
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not save the pool.';
		}
		busy = false;
	}
</script>

<div class="mt-3 grid gap-3 border-t border-line pt-3 md:grid-cols-2">
	<form class="flex flex-wrap items-center gap-2" onsubmit={(e) => (e.preventDefault(), void labelAll(false))}>
		<label class="min-w-40 flex-1">
			<span class="sr-only">Label as key=value</span>
			<input class="{field} font-mono" list="label-keys" bind:value={text} placeholder="room=12 or broken" maxlength="80" autocomplete="off" spellcheck="false" />
		</label>
		<datalist id="label-keys">{#each keys as k (k)}<option value={k}></option>{/each}</datalist>
		<button type="submit" class={btnGhost} disabled={busy || !text.trim()}><Tag class="size-4" aria-hidden="true" />Set label</button>
		<button type="button" class={btnGhost} disabled={busy || !text.trim()} onclick={() => void labelAll(true)}>Remove label</button>
	</form>
	<form class="flex flex-wrap items-center gap-2" onsubmit={(e) => (e.preventDefault(), void savePool())}>
		<label class="min-w-40 flex-1">
			<span class="sr-only">Pool name</span>
			<input class={field} list="pool-names" bind:value={pool} placeholder="Pool name (new or existing)" maxlength="40" autocomplete="off" />
		</label>
		<datalist id="pool-names">{#each poolNames as n (n)}<option value={n}></option>{/each}</datalist>
		<button type="submit" class={btnPrimary} disabled={busy || !pool.trim()}><Layers class="size-4" aria-hidden="true" />Add to pool</button>
	</form>
</div>
{#if error}<p class="text-bad mt-2 text-[13px]" role="alert">{error}</p>{:else if note}<p class="text-ok mt-2 text-[13px]" role="status">{note}</p>{/if}
