<script lang="ts">
	import { Ban, CircleCheck, Play, Wrench, ArrowDownToLine, Moon, Sun, X } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { NodeRow, NodeState } from '#lib/api/schema.ts';
	import NodeOrg from '#lib/components/NodeOrg.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import { atLeast } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnGhost, btnPrimary, field } from '#lib/ui.ts';

	interface Props {
		ids: string[];
		ondone?: () => void;
	}
	let { ids, ondone }: Props = $props();
	const canAct = $derived(atLeast(roleNow(), 'operator'));
	const rows = $derived(ids.map((i) => live.nodes[i]).filter((n): n is NodeRow => !!n));
	const pending = $derived(rows.filter((n) => n.state === 'pending'));
	const others = $derived(rows.filter((n) => n.state !== 'pending' && n.state !== 'active'));
	let reason = $state('');
	let busy = $state(false);
	let error = $state('');
	let ask = $state<{ state: NodeState; label: string } | null>(null);
	let askSleep = $state(false);
	let askOpen = $state(false);

	async function run(state: NodeState, only: NodeRow[] = rows) {
		busy = true;
		error = '';
		try {
			const r = await api.setNodeState(only.map((n) => n.node_id), state, reason.trim());
			for (const n of r.nodes) live.nodes[n.node_id] = n;
			reason = '';
			askOpen = false;
			ondone?.();
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not change the state.';
		}
		busy = false;
	}
	async function power(action: 'sleep' | 'wake') {
		const only = action === 'sleep' ? rows.filter((n) => n.power === 'awake' && n.online) : rows.filter((n) => n.power === 'asleep');
		busy = true;
		error = '';
		try {
			const r = await api.powerNodes(only.map((n) => n.node_id), action);
			for (const n of r.nodes) live.nodes[n.node_id] = n;
			askSleep = false;
			ondone?.();
		} catch (e) {
			error = e instanceof ApiError ? e.message : `Could not ${action} the PCs.`;
		}
		busy = false;
	}
	const awake = $derived(rows.filter((n) => n.power === 'awake' && n.online));
	const asleep = $derived(rows.filter((n) => n.power === 'asleep'));
	function go(state: NodeState, label: string, risky: boolean) {
		if (!risky) return void run(state);
		ask = { state, label };
		askOpen = true;
	}
	const b = `${btnGhost} px-2.5 py-1.5`;
</script>

<div class="bg-surface border-line rounded-lg border p-3" role="region" aria-label="Actions for {ids.length} selected PC{ids.length === 1 ? '' : 's'}">
	<div class="flex flex-wrap items-center gap-2">
		<p class="mr-1 text-[13px] font-medium"><span class="num">{ids.length}</span> selected</p>
		{#if canAct}
			<button type="button" class={btnPrimary} disabled={busy || !pending.length} title={pending.length ? '' : 'None of the selected PCs is pending'} onclick={() => run('active', pending)}>
				<CircleCheck class="size-4" aria-hidden="true" />Approve{pending.length ? ` (${pending.length})` : ''}
			</button>
			<button type="button" class={b} disabled={busy} onclick={() => go('cordoned', 'Cordon', false)}><Ban class="size-4" aria-hidden="true" />Cordon</button>
			<button type="button" class={b} disabled={busy} onclick={() => go('draining', 'Drain', true)}><ArrowDownToLine class="size-4" aria-hidden="true" />Drain</button>
			<button type="button" class={b} disabled={busy} onclick={() => go('maintenance', 'Start maintenance', true)}><Wrench class="size-4" aria-hidden="true" />Maintenance</button>
			<button type="button" class={b} disabled={busy || !others.length} title={others.length ? '' : 'The selected PCs are already active or pending'} onclick={() => run('active', others)}>
				<Play class="size-4" aria-hidden="true" />Activate
			</button>
			<button type="button" class={b} disabled={busy || !awake.length} title={awake.length ? 'Put the PCs to sleep to save power' : 'None of the selected PCs is awake and connected'} onclick={() => (askSleep = true)}><Moon class="size-4" aria-hidden="true" />Sleep</button>
			<button type="button" class={b} disabled={busy || !asleep.length} title={asleep.length ? 'Wake the PCs with Wake-on-LAN' : 'None of the selected PCs is asleep'} onclick={() => void power('wake')}><Sun class="size-4" aria-hidden="true" />Wake{asleep.length ? ` (${asleep.length})` : ''}</button>
			<label class="min-w-40 flex-1">
				<span class="sr-only">Reason (recorded with the change)</span>
				<input bind:value={reason} maxlength="200" placeholder="Reason (recorded)" class={field} />
			</label>
		{:else}
			<p class="text-muted-foreground text-[13px]">Changing a PC's state needs the operator role. You are signed in as {roleNow()}.</p>
		{/if}
		<button type="button" class="text-muted-foreground hover:text-foreground ml-auto inline-flex items-center gap-1 text-[13px]" onclick={() => ondone?.()}>
			<X class="size-4" aria-hidden="true" />Clear
		</button>
	</div>
	{#if canAct}<NodeOrg {ids} />{/if}
	{#if error}<p class="text-bad mt-2 text-[13px]" role="alert">{error}</p>{/if}
</div>

<ConfirmDialog
	bind:open={askOpen}
	title="{ask?.label} {rows.length} PC{rows.length === 1 ? '' : 's'}?"
	body={ask?.state === 'draining'
		? 'Each PC finishes the tasks it holds, then stops taking new ones (it becomes cordoned).'
		: 'The PCs take no new tasks until someone activates them again.'}
	confirm={ask?.label ?? ''}
	{busy}
	onconfirm={() => ask && run(ask.state)}
/>

<ConfirmDialog
	bind:open={askSleep}
	title="Sleep {awake.length} PC{awake.length === 1 ? '' : 's'}?"
	body="Each PC suspends now. Anything running on it stops, and it comes back only when you wake it (Wake-on-LAN) or someone presses its power button."
	confirm="Sleep"
	{busy}
	onconfirm={() => power('sleep')}
/>
