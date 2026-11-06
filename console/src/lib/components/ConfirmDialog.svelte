<script lang="ts">
	import { btnDanger, btnGhost } from '#lib/ui.ts';

	interface Props {
		open: boolean;
		title: string;
		body: string;
		confirm: string;
		busy?: boolean;
		onconfirm: () => void;
	}
	let { open = $bindable(), title, body, confirm, busy = false, onconfirm }: Props = $props();
	let dlg: HTMLDialogElement;

	// Native <dialog>: focus trap, Escape and backdrop come for free.
	$effect(() => {
		if (open && !dlg.open) dlg.showModal();
		if (!open && dlg.open) dlg.close();
	});
</script>

<dialog
	bind:this={dlg}
	onclose={() => (open = false)}
	aria-labelledby="confirm-title"
	class="bg-surface text-foreground border-line m-auto w-[min(26rem,calc(100vw-2rem))] rounded-lg border p-5 backdrop:bg-black/50"
>
	<h2 id="confirm-title" class="text-[15px] font-semibold">{title}</h2>
	<p class="text-muted-foreground mt-1.5" aria-live="polite">{body}</p>
	<div class="mt-5 flex justify-end gap-2">
		<button type="button" class={btnGhost} onclick={() => (open = false)}>Keep it</button>
		<button type="button" class={btnDanger} disabled={busy} onclick={onconfirm}>{busy ? 'Working…' : confirm}</button>
	</div>
</dialog>
