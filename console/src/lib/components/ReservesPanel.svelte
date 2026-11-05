<script lang="ts">
	// host name, plus the virtual-node suffix so PCs sharing one host stay distinct
	const pcName = (id: string) => {
		const h = live.nodes[id]?.host ?? id;
		const v = /\/(v\d+)$/.exec(id);
		return v ? `${h.split('.')[0]} ${v[1]}` : h;
	};
	import { Plus, ShieldCheck, Trash2 } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { ReserveRow } from '#lib/api/schema.ts';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { reserveText } from '#lib/org.ts';
	import { btnGhost, btnPrimary, card, field } from '#lib/ui.ts';

	let { canEdit }: { canEdit: boolean } = $props();
	let rows = $state<ReserveRow[] | null>(null);
	let error = $state('');
	let sel = $state('');
	let keepN = $state<number | null>(null);
	let keepPct = $state<number | null>(null);
	let note = $state('');
	let busy = $state(false);

	const load = () => api.reserves().then((r) => ((rows = r.reserves), (error = ''))).catch((e) => (error = e instanceof ApiError ? e.message : 'Could not load reserves.'));
	// "held" changes as jobs start and stop, so re-read every few seconds
	$effect(() => {
		void load();
		const t = setInterval(load, 5000);
		return () => clearInterval(t);
	});

	async function add(e: SubmitEvent) {
		e.preventDefault();
		if (!(keepN && keepN > 0) && !(keepPct && keepPct > 0)) return void (error = 'Enter how many PCs or what percent to keep free.');
		busy = true;
		try {
			await api.addReserve({ selector: sel.trim(), keep_n: keepN ?? 0, keep_pct: keepPct ?? 0, note: note.trim() });
			sel = note = '';
			keepN = keepPct = null;
			await load();
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not add the reserve.';
		}
		busy = false;
	}
	async function del(id: number) {
		try {
			await api.deleteReserve(id);
			await load();
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not delete the reserve.';
		}
	}
</script>

<section aria-label="Reserves">
	<h2 class="sr-only">Reserves</h2>
	<p class="text-muted-foreground mt-0.5 text-[13px]">Keep PCs free of jobs. Example: keep 10 PCs in Lab A free for students. The head picks which PCs to hold and keeps that choice stable.</p>
	{#if error}<p class="text-bad mt-2 text-[13px]" role="alert">{error}</p>{/if}
	<div class="{card} mt-3">
		{#if !rows}
			<div class="h-16 animate-pulse" aria-busy="true"></div>
		{:else if rows.length === 0}
			<p class="text-muted-foreground px-4 py-5 text-center">No reserves. Jobs may use every PC.</p>
		{:else}
			<ul class="divide-line divide-y">
				{#each rows as r (r.id)}
					<li class="px-4 py-3">
						<div class="flex items-start justify-between gap-3">
							<div class="min-w-0">
								<p class="flex items-start gap-1.5 break-words"><ShieldCheck class="text-ok mt-0.5 size-4 shrink-0" aria-hidden="true" />{reserveText(r)}</p>
								<p class="text-muted-foreground mt-0.5 font-mono text-xs break-all">{r.selector || '(all PCs)'}</p>
								{#if r.note}<p class="text-muted-foreground mt-0.5 text-xs">{r.note}</p>{/if}
							</div>
							{#if canEdit}<button type="button" class="{btnGhost} px-2.5" aria-label="Delete reserve {r.id}" onclick={() => void del(r.id)}><Trash2 class="size-4" aria-hidden="true" /></button>{/if}
						</div>
						<p class="mt-2 text-xs"><span class="num font-semibold">{r.held.length}</span> held free right now{r.held.length ? ':' : '.'}</p>
						{#if r.held.length}
							<ul class="mt-1 flex flex-wrap gap-1">
								{#each r.held as id (id)}<li class="border-line rounded-md border px-1.5 py-0.5 text-[11px]">{pcName(id)}</li>{/each}
							</ul>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</div>
	{#if canEdit}
		<form onsubmit={add} class="{card} mt-3 space-y-3 p-4">
			<SelectorBar bind:value={sel} label="Which PCs" id="res-sel" emptyMeans="Every PC" />
			<div class="grid gap-3 sm:grid-cols-[7rem_7rem_1fr_auto] sm:items-end">
				<label class="block"><span class="mb-1 block text-xs font-medium">Keep N free</span><input class={field} type="number" min="0" step="1" inputmode="numeric" bind:value={keepN} /></label>
				<label class="block"><span class="mb-1 block text-xs font-medium">Keep % free</span><input class={field} type="number" min="0" max="100" step="1" inputmode="numeric" bind:value={keepPct} /></label>
				<label class="block"><span class="mb-1 block text-xs font-medium">Note (optional)</span><input class={field} bind:value={note} maxlength="200" placeholder="For students" /></label>
				<button type="submit" class={btnPrimary} disabled={busy}><Plus class="size-4" aria-hidden="true" />Add reserve</button>
			</div>
		</form>
	{:else}
		<p class="text-muted-foreground mt-2 text-xs">Changing reserves needs the operator role.</p>
	{/if}
</section>
