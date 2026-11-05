<script lang="ts">
	import { Plus, Trash2, X } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { PoolRow } from '#lib/api/schema.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import { live, loadFleet } from '#lib/live.svelte.ts';
	import { plural } from '#lib/org.ts';
	import { btnGhost, btnPrimary, card, field } from '#lib/ui.ts';

	let { canEdit }: { canEdit: boolean } = $props();
	let pools = $state<PoolRow[] | null>(null);
	let error = $state('');
	let name = $state('');
	let sel = $state('');
	let matched = $state<string[] | null>(null);
	let busy = $state(false);
	let addSel = $state<Record<string, string>>({});
	let addMatched = $state<Record<string, string[] | null>>({});
	let delName = $state<string | null>(null);
	let delOpen = $state(false);

	const load = () => api.pools().then((r) => {
		for (const p of r.pools) (addSel[p.name] ??= ''), (addMatched[p.name] ??= null);
		pools = r.pools;
		error = '';
	}).catch((e) => (error = e instanceof ApiError ? e.message : 'Could not load pools.'));
	$effect(() => void load());

	async function save(n: string, members: string[], note = '') {
		busy = true;
		try {
			await api.putPool(n, members, note);
			await Promise.all([load(), loadFleet()]);
			error = '';
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not save the pool.';
		}
		busy = false;
	}
	async function create(e: SubmitEvent) {
		e.preventDefault();
		if (!name.trim()) return void (error = 'Give the pool a name.');
		if (!matched?.length) return void (error = 'Type a selector that matches at least one PC.');
		await save(name.trim(), matched);
		if (!error) name = sel = '';
	}
	async function remove() {
		busy = true;
		try {
			await api.deletePool(delName!);
			delOpen = false;
			await Promise.all([load(), loadFleet()]);
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not delete the pool.';
			delOpen = false;
		}
		busy = false;
	}
</script>

<section aria-label="Pools">
	<h2 class="sr-only">Pools</h2>
	<p class="text-muted-foreground mt-0.5 text-[13px]">A named group of PCs you pick by hand. Use it in selectors as <code class="font-mono text-xs">pool=night</code>.</p>
	{#if error}<p class="text-bad mt-2 text-[13px]" role="alert">{error}</p>{/if}
	<div class="mt-3 space-y-3">
		{#if !pools}
			<div class="{card} h-16 animate-pulse" aria-busy="true"></div>
		{:else if pools.length === 0}
			<p class="text-muted-foreground {card} px-4 py-5 text-center">No pools yet. Create one from a selector below, or select PCs on the Nodes page.</p>
		{/if}
		{#each pools ?? [] as p (p.name)}
			<div class="{card} p-4">
				<div class="flex items-start justify-between gap-3">
					<div class="min-w-0">
						<p class="font-medium">{p.name} <span class="text-muted-foreground num text-xs font-normal">{plural(p.members.length, 'PC')}</span></p>
						{#if p.note}<p class="text-muted-foreground text-xs">{p.note}</p>{/if}
					</div>
					{#if canEdit}<button type="button" class="{btnGhost} px-2.5" aria-label="Delete pool {p.name}" onclick={() => ((delName = p.name), (delOpen = true))}><Trash2 class="size-4" aria-hidden="true" /></button>{/if}
				</div>
				<ul class="mt-2 flex flex-wrap gap-1">
					{#each p.members as id (id)}
						<li class="border-line inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 text-[11px]">
							{live.nodes[id]?.host ?? id}
							{#if canEdit}<button type="button" class="text-muted-foreground hover:text-bad" disabled={busy} aria-label="Remove {live.nodes[id]?.host ?? id} from {p.name}" onclick={() => void save(p.name, p.members.filter((m) => m !== id), p.note)}><X class="size-3" aria-hidden="true" /></button>{/if}
						</li>
					{:else}<li class="text-muted-foreground text-xs">No members.</li>{/each}
				</ul>
				{#if canEdit}
					<div class="mt-3">
						<SelectorBar bind:value={addSel[p.name]} bind:matched={addMatched[p.name]} label="Add PCs matching" id="pool-add-{p.name}" emptyMeans="Type a selector to add PCs" placeholder="lab=B state=active">
							{#snippet actions()}
								<button type="button" class="{btnGhost} self-start" disabled={busy || !addMatched[p.name]?.length} onclick={async () => { await save(p.name, [...new Set([...p.members, ...(addMatched[p.name] ?? [])])], p.note); addSel[p.name] = ''; }}>Add</button>
							{/snippet}
						</SelectorBar>
					</div>
				{/if}
			</div>
		{/each}
	</div>
	{#if canEdit}
		<form onsubmit={create} class="{card} mt-3 space-y-3 p-4">
			<SelectorBar bind:value={sel} bind:matched label="Create a pool from PCs matching" id="pool-new" emptyMeans="Type a selector to choose members" />
			<div class="grid gap-3 sm:grid-cols-[1fr_auto] sm:items-end">
				<label class="block"><span class="mb-1 block text-xs font-medium">Pool name</span><input class={field} bind:value={name} maxlength="40" placeholder="night" autocomplete="off" /></label>
				<button type="submit" class={btnPrimary} disabled={busy}><Plus class="size-4" aria-hidden="true" />Create pool</button>
			</div>
		</form>
	{:else}
		<p class="text-muted-foreground mt-2 text-xs">Changing pools needs the operator role.</p>
	{/if}
</section>

<ConfirmDialog bind:open={delOpen} title="Delete pool {delName}?" body="The PCs stay as they are. Selectors that use this pool will match nothing." confirm="Delete pool" {busy} onconfirm={remove} />
