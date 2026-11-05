<script lang="ts">
	import { Plus, Trash2 } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { CapRow } from '#lib/api/schema.ts';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import { capText } from '#lib/org.ts';
	import { btnGhost, btnPrimary, card, field } from '#lib/ui.ts';

	let { canEdit }: { canEdit: boolean } = $props();
	let caps = $state<CapRow[] | null>(null);
	let error = $state('');
	let sel = $state('');
	let threads = $state<number | null>(4);
	let note = $state('');
	let busy = $state(false);

	const load = () => api.caps().then((r) => ((caps = r.caps), (error = ''))).catch((e) => (error = e instanceof ApiError ? e.message : 'Could not load caps.'));
	$effect(() => void load());

	async function add(e: SubmitEvent) {
		e.preventDefault();
		if (!threads || threads < 1) return void (error = 'Max threads must be 1 or more.');
		busy = true;
		try {
			await api.addCap({ selector: sel.trim(), max_threads: threads, note: note.trim() });
			sel = note = '';
			await load();
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not add the cap.';
		}
		busy = false;
	}
	async function del(id: number) {
		try {
			await api.deleteCap(id);
			await load();
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not delete the cap.';
		}
	}
</script>

<section aria-label="Thread caps">
	<h2 class="font-medium">Thread caps</h2>
	<p class="text-muted-foreground mt-0.5 text-[13px]">Limit how many threads matching PCs give to jobs, for example so a lab stays usable for the person sitting there.</p>
	{#if error}<p class="text-bad mt-2 text-[13px]" role="alert">{error}</p>{/if}
	<div class="{card} mt-3">
		{#if !caps}
			<div class="h-16 animate-pulse" aria-busy="true"></div>
		{:else if caps.length === 0}
			<p class="text-muted-foreground px-4 py-5 text-center">No caps. Every PC gives all its threads.</p>
		{:else}
			<ul class="divide-line divide-y">
				{#each caps as c (c.id)}
					<li class="flex items-start justify-between gap-3 px-4 py-3">
						<div class="min-w-0">
							<p class="break-words">{capText(c)}</p>
							<p class="text-muted-foreground mt-0.5 font-mono text-xs break-all">{c.selector || '(all PCs)'}</p>
							{#if c.note}<p class="text-muted-foreground mt-0.5 text-xs">{c.note}</p>{/if}
						</div>
						{#if canEdit}<button type="button" class="{btnGhost} px-2.5" aria-label="Delete cap {c.id}" onclick={() => void del(c.id)}><Trash2 class="size-4" aria-hidden="true" /></button>{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</div>
	{#if canEdit}
		<form onsubmit={add} class="{card} mt-3 space-y-3 p-4">
			<SelectorBar bind:value={sel} label="Which PCs" id="cap-sel" emptyMeans="Every PC" />
			<div class="grid gap-3 sm:grid-cols-[8rem_1fr_auto] sm:items-end">
				<label class="block"><span class="mb-1 block text-xs font-medium">Max threads</span><input class={field} type="number" min="1" step="1" inputmode="numeric" bind:value={threads} /></label>
				<label class="block"><span class="mb-1 block text-xs font-medium">Note (optional)</span><input class={field} bind:value={note} maxlength="200" placeholder="Why this cap exists" /></label>
				<button type="submit" class={btnPrimary} disabled={busy}><Plus class="size-4" aria-hidden="true" />Add cap</button>
			</div>
		</form>
	{:else}
		<p class="text-muted-foreground mt-2 text-xs">Changing caps needs the operator role.</p>
	{/if}
</section>
