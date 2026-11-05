<script lang="ts">
	import { Check, Copy, KeyRound } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { JoinCode } from '#lib/api/schema.ts';
	import { datetime } from '#lib/format.ts';
	import { btnGhost, btnPrimary, field } from '#lib/ui.ts';

	let open = $state(false);
	let dlg = $state<HTMLDialogElement>();
	let uses = $state(50);
	let minutes = $state(120);
	let auto = $state(false);
	let head = $state(typeof location === 'undefined' ? '' : location.hostname);
	let busy = $state(false);
	let error = $state('');
	let made = $state<JoinCode | null>(null);
	let copied = $state('');

	$effect(() => {
		if (open && !dlg?.open) dlg?.showModal();
		if (!open && dlg?.open) dlg.close();
	});
	const cmd = $derived(made ? `CK_JOIN_CODE=${made.code} ck agent --head ${head || '<head ip>'}` : '');

	async function create(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = '';
		try {
			made = await api.createJoinCode({ uses, ttl_min: minutes, auto_approve: auto });
		} catch (err) {
			error = err instanceof ApiError ? err.message : 'Could not create the code.';
		}
		busy = false;
	}
	async function copy(what: string, text: string) {
		try {
			await navigator.clipboard.writeText(text);
			copied = what;
			setTimeout(() => (copied = ''), 2000);
		} catch {
			error = 'Copying is blocked here. Select the text and copy it by hand.';
		}
	}
	function close() {
		open = false;
		made = null; // the code is gone for good once the dialog closes
		error = '';
	}
</script>

<button type="button" class={btnGhost} onclick={() => (open = true)}><KeyRound class="size-4" aria-hidden="true" />Join codes</button>

<dialog
	bind:this={dlg}
	onclose={close}
	aria-labelledby="jc-title"
	class="bg-surface text-foreground border-line m-auto w-[min(30rem,calc(100vw-2rem))] rounded-lg border p-5 backdrop:bg-black/50"
>
	<h2 id="jc-title" class="text-[15px] font-semibold">{made ? 'Join code created' : 'New join code'}</h2>
	{#if made}
		<p class="text-warn mt-1.5 text-[13px]" role="status">This code is shown once. Copy it now.</p>
		<div class="mt-3 flex items-center gap-2">
			<code class="bg-raised border-line num flex-1 rounded-lg border px-3 py-2 font-mono text-lg tracking-wider select-all">{made.code}</code>
			<button type="button" class={btnGhost} onclick={() => copy('code', made!.code)} aria-label="Copy the code">
				{#if copied === 'code'}<Check class="size-4" aria-hidden="true" />Copied{:else}<Copy class="size-4" aria-hidden="true" />Copy{/if}
			</button>
		</div>
		<p class="text-muted-foreground mt-2 text-xs">{made.uses} use{made.uses === 1 ? '' : 's'}, expires {datetime(made.expires_ms)}{made.auto_approve ? ', PCs are approved on joining' : ', PCs wait for approval'}.</p>
		<p class="mt-4 text-[13px] font-medium">Run on a lab PC</p>
		<label class="mt-1 block text-xs">
			<span class="text-muted-foreground">Head address</span>
			<input bind:value={head} class="{field} mt-1" />
		</label>
		<div class="mt-2 flex items-start gap-2">
			<code class="bg-raised border-line min-w-0 flex-1 rounded-lg border px-3 py-2 font-mono text-xs break-all select-all">{cmd}</code>
			<button type="button" class={btnGhost} onclick={() => copy('cmd', cmd)} aria-label="Copy the command">
				{#if copied === 'cmd'}<Check class="size-4" aria-hidden="true" />{:else}<Copy class="size-4" aria-hidden="true" />{/if}
			</button>
		</div>
		<div class="mt-5 flex justify-end"><button type="button" class={btnPrimary} onclick={close}>Done</button></div>
	{:else}
		<form onsubmit={create} class="mt-3 grid gap-3">
			<label class="text-[13px]"><span class="font-medium">Uses</span>
				<input type="number" min="1" max="500" required bind:value={uses} class="{field} mt-1" />
			</label>
			<label class="text-[13px]"><span class="font-medium">Minutes valid</span>
				<input type="number" min="1" max="10080" required bind:value={minutes} class="{field} mt-1" />
			</label>
			<label class="flex items-start gap-2 text-[13px]">
				<input type="checkbox" bind:checked={auto} class="mt-0.5" />
				<span><span class="font-medium">Approve PCs automatically</span><br /><span class="text-muted-foreground">Otherwise each PC waits as pending until an operator approves it.</span></span>
			</label>
			{#if error}<p class="text-bad text-[13px]" role="alert">{error}</p>{/if}
			<div class="flex justify-end gap-2">
				<button type="button" class={btnGhost} onclick={close}>Cancel</button>
				<button type="submit" class={btnPrimary} disabled={busy}>{busy ? 'Creating…' : 'Create code'}</button>
			</div>
		</form>
	{/if}
</dialog>
