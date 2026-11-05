<script lang="ts">
	import { api, ApiError } from '#lib/api/client.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import { live } from '#lib/live.svelte.ts';

	// HA-07: the planned switchover (Make head). One dialog for every place that offers it.
	let { target = null, open = $bindable(false), onnote }: { target?: string | null; open?: boolean; onnote?: (text: string) => void } = $props();
	let busy = $state(false);
	const host = $derived(target ? (live.nodes[target]?.host ?? target) : '');

	async function go() {
		if (!target) return;
		busy = true;
		try {
			const r = await api.switchover(target);
			live.handover = { to: target, at: Date.now() };
			onnote?.(`Handing over to ${r.to}. Work pauses for a few seconds while the PCs reconnect.`);
		} catch (e) {
			onnote?.(e instanceof ApiError ? e.message : 'The head did not answer.');
		} finally {
			busy = false;
			open = false;
		}
	}
</script>

<ConfirmDialog
	bind:open
	title="Make {host} the head?"
	body="This head stops handing out work, waits until that standby has every change, then steps down to be its standby. PCs and running jobs move over by themselves; work pauses for a few seconds."
	confirm="Make head"
	{busy}
	onconfirm={go}
/>
