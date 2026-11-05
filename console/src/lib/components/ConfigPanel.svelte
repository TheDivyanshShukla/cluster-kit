<script lang="ts">
	import { Download, FileUp, Plus, Minus, Pencil } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { changeKind, changeText } from '#lib/org.ts';
	import { btnGhost, btnPrimary, card } from '#lib/ui.ts';

	// Admin only (the parent decides). Export the whole setup; import with a dry run first.
	let changes = $state<string[] | null>(null);
	let cfg = $state<unknown>(null);
	let fileName = $state('');
	let error = $state('');
	let done = $state('');
	let busy = $state(false);

	async function download() {
		error = '';
		try {
			const url = URL.createObjectURL(new Blob([await api.configText()], { type: 'application/json' }));
			const a = Object.assign(document.createElement('a'), { href: url, download: 'cluster-kit-config.json' });
			a.click();
			URL.revokeObjectURL(url);
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not export the setup.';
		}
	}
	async function pick(e: Event) {
		const f = (e.currentTarget as HTMLInputElement).files?.[0];
		changes = cfg = null;
		error = done = '';
		if (!f) return;
		fileName = f.name;
		busy = true;
		try {
			cfg = JSON.parse(await f.text());
			changes = (await api.configPreview(cfg)).changes;
		} catch (x) {
			cfg = null;
			error = x instanceof ApiError ? x.message : 'That file is not valid JSON.';
		}
		busy = false;
	}
	async function apply() {
		busy = true;
		try {
			await api.configApply(cfg);
			done = `Applied ${changes?.length ?? 0} change${changes?.length === 1 ? '' : 's'}.`;
			changes = cfg = null;
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not apply the setup.';
		}
		busy = false;
	}
	const mark = { add: { i: Plus, c: 'text-ok' }, remove: { i: Minus, c: 'text-bad' }, change: { i: Pencil, c: 'text-warn' } };
</script>

<section aria-label="Setup export and import">
	<h2 class="font-medium">Setup backup</h2>
	<p class="text-muted-foreground mt-0.5 text-[13px]">Export labs, seats, labels, pools, caps and reserves as one JSON file. Importing shows what would change before anything is applied.</p>
	<div class="{card} mt-3 flex flex-wrap items-center gap-2 p-4">
		<button type="button" class={btnGhost} onclick={() => void download()}><Download class="size-4" aria-hidden="true" />Download JSON</button>
		<label class="{btnGhost} cursor-pointer has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-ring">
			<FileUp class="size-4" aria-hidden="true" />Choose file to import
			<input type="file" accept="application/json,.json" class="sr-only" onchange={pick} />
		</label>
		{#if fileName && cfg}<span class="text-muted-foreground text-xs">{fileName}</span>{/if}
	</div>
	{#if error}<p class="text-bad mt-2 text-[13px]" role="alert">{error}</p>{/if}
	{#if done}<p class="text-ok mt-2 text-[13px]" role="status">{done}</p>{/if}
	{#if changes}
		<div class="{card} mt-3 p-4">
			{#if changes.length === 0}
				<p class="text-muted-foreground">Nothing would change: the file matches the current setup.</p>
			{:else}
				<p class="mb-2 text-[13px] font-medium"><span class="num">{changes.length}</span> change{changes.length === 1 ? '' : 's'} if you apply this file</p>
				<ul class="space-y-1 font-mono text-xs">
					{#each changes as l, i (i)}
						{@const m = mark[changeKind(l)]}
						<li class="flex items-start gap-2"><m.i class="{m.c} mt-0.5 size-3.5 shrink-0" aria-hidden="true" /><span class="break-words">{changeText(l)}</span></li>
					{/each}
				</ul>
				<div class="mt-3 flex justify-end"><button type="button" class={btnPrimary} disabled={busy} onclick={() => void apply()}>Apply {changes.length} change{changes.length === 1 ? '' : 's'}</button></div>
			{/if}
		</div>
	{/if}
</section>
