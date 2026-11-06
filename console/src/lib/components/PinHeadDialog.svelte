<script lang="ts">
	import { tick } from 'svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Candidate } from '#lib/api/schema.ts';
	import { pct } from '#lib/format.ts';
	import { rankFor } from '#lib/hier.ts';
	import { btnGhost, btnPrimary } from '#lib/ui.ts';

	// Pick the PC that hosts a lab's lab head. The list is that lab's PCs ranked by head suitability (GET /ha/candidates).
	interface Props {
		open: boolean;
		lab: string;
		current: string | null;
		ondone: () => void;
	}
	let { open = $bindable(), lab, current, ondone }: Props = $props();
	let dlg: HTMLDialogElement;
	let cands = $state<Candidate[] | null>(null);
	let pick = $state('');
	let busy = $state(false);
	let err = $state('');

	$effect(() => {
		if (open && !dlg.open) {
			dlg.showModal();
			err = '';
			cands = null;
			api.candidates().then(
				(r) => {
					cands = rankFor(lab, r.candidates);
					pick = (cands.find((c) => c.eligible && c.node_id !== current) ?? cands.find((c) => c.node_id !== current))?.node_id ?? '';
						// the list arrives after the dialog opens: move focus from Cancel to the chosen PC
						void tick().then(() => dlg.querySelector<HTMLInputElement>('input:checked')?.focus());
				},
				(e) => ((cands = []), (err = e instanceof ApiError ? e.message : 'Could not rank this lab\'s PCs.'))
			);
		}
		if (!open && dlg.open) dlg.close();
	});
	async function go() {
		busy = true;
		err = '';
		try {
			await api.pinLabHead(lab, pick);
			open = false;
			ondone();
		} catch (e) {
			err = e instanceof ApiError ? e.message : 'Could not pin the lab head.';
		}
		busy = false;
	}
</script>

<dialog bind:this={dlg} onclose={() => (open = false)} aria-labelledby="pin-title" class="bg-surface text-foreground border-line m-auto w-[min(30rem,calc(100vw-2rem))] rounded-lg border p-5 backdrop:bg-black/50">
	<h2 id="pin-title" class="text-[15px] font-semibold">{current ? 'Move' : 'Pin'} the lab head of {lab}</h2>
	<p class="text-muted-foreground mt-1.5 text-[13px]">{current ? 'The current lab head hands its PCs back and exits; the new one starts.' : 'This PC hosts the lab head and the lab\'s other PCs move under it.'} Best suited first.</p>
	{#if cands === null}
		<div class="bg-raised mt-3 h-24 animate-pulse rounded-lg" aria-busy="true" aria-label="Ranking PCs"></div>
	{:else if cands.length === 0}
		<p class="text-muted-foreground mt-3 text-[13px]">No PC of {lab} is connected, so none can host a lab head.</p>
	{:else}
		<fieldset class="mt-3 max-h-72 overflow-y-auto">
			<legend class="sr-only">PC to host the lab head</legend>
			<ul class="divide-line divide-y">
				{#each cands as c (c.node_id)}
					<li>
						<label class="hover:bg-raised flex min-h-11 cursor-pointer items-center gap-3 rounded-md px-2 py-1.5 has-[:disabled]:cursor-not-allowed has-[:disabled]:opacity-60 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-ring">
							<input type="radio" name="pin-pc" class="accent-primary size-4" value={c.node_id} bind:group={pick} disabled={!c.eligible || c.node_id === current} />
							<span class="min-w-0 flex-1">
								<span class="block truncate font-medium">{c.host}{c.node_id === current ? ' (current)' : ''}</span>
								<span class="text-muted-foreground block truncate text-xs">{c.node_id} · {c.eligible ? `speed ${pct(c.parts.speed)} · memory ${pct(c.parts.memory)} · stability ${pct(c.parts.stability)}` : c.why || 'not eligible'}</span>
							</span>
							<span class="num text-[13px] font-medium" title="Head suitability, 0 to 100">{Math.round(c.score)}</span>
						</label>
					</li>
				{/each}
			</ul>
		</fieldset>
	{/if}
	{#if err}<p class="text-bad mt-3 text-[13px]" role="alert">{err}</p>{/if}
	<div class="mt-5 flex justify-end gap-2">
		<button type="button" class={btnGhost} onclick={() => (open = false)}>Cancel</button>
		<button type="button" class={btnPrimary} disabled={busy || !pick} onclick={go}>{busy ? 'Working…' : current ? 'Move lab head' : 'Pin lab head'}</button>
	</div>
</dialog>
