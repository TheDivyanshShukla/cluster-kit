<script lang="ts">
	import { CircleCheck, Search, Unplug } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import AvailChips from '#lib/components/AvailChips.svelte';
	import LabelChips from '#lib/components/LabelChips.svelte';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import JoinCodes from '#lib/components/JoinCodes.svelte';
	import NodeActions from '#lib/components/NodeActions.svelte';
	import NodeStateChip from '#lib/components/NodeStateChip.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { bytes, count, pct, rate } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { toItems } from '#lib/map.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnGhost, btnPrimary } from '#lib/ui.ts';

	let q = $state('');
	let sel = $state('');
	let matched = $state<string[] | null>(null);
	let selected = $state<string[]>([]);
	let err = $state('');
	let busy = $state(false);
	const canAct = $derived(atLeast(roleNow(), 'operator'));
	const all = $derived(toItems(live.nodes, live.cluster?.workers ?? []));
	const pending = $derived(all.filter((i) => i.node.state === 'pending'));
	const labName = (id: number | null) => live.labs.find((l) => l.id === id)?.name;
	const rows = $derived(
		all.filter((i) => {
			if (matched && !matched.includes(i.node.node_id)) return false;
			const t = q.trim().toLowerCase();
			return !t || [i.node.host, i.node.addr, i.node.node_id, i.node.state, i.node.inventory?.cpu_model].some((v) => v?.toLowerCase().includes(t));
		})
	);
	const allOn = $derived(rows.length > 0 && rows.every((r) => selected.includes(r.node.node_id)));

	async function unlabel(id: string, key: string) {
		err = '';
		try {
			live.nodes[id] = (await api.setLabels(id, { [key]: null })).node;
		} catch (e) {
			err = e instanceof ApiError ? e.message : 'Could not remove the label.';
		}
	}

	async function approve(ids: string[]) {
		busy = true;
		err = '';
		try {
			for (const n of (await api.setNodeState(ids, 'active')).nodes) live.nodes[n.node_id] = n;
			selected = selected.filter((s) => !ids.includes(s));
		} catch (e) {
			err = e instanceof ApiError ? e.message : 'Could not approve.';
		}
		busy = false;
	}
</script>

<svelte:head><title>Nodes · Cluster Kit</title></svelte:head>

<div class="flex flex-wrap items-end justify-between gap-3">
	<div>
		<h1 class="text-xl font-semibold tracking-tight">Nodes</h1>
		<p class="text-muted-foreground mt-1">Every PC registered with this head.</p>
	</div>
	<div class="flex w-full flex-wrap items-center gap-2 sm:w-auto">
		{#if canAct}<JoinCodes />{/if}
		<label class="bg-surface border-line focus-within:ring-ring flex min-w-0 flex-1 items-center gap-2 rounded-lg border px-3 py-1.5 focus-within:ring-2 sm:w-72 sm:flex-none">
			<Search class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />
			<span class="sr-only">Filter nodes</span>
			<input bind:value={q} placeholder="Filter by name, address, state" class="w-full bg-transparent outline-none" />
		</label>
	</div>
</div>

<div class="mt-4 max-w-3xl">
	<SelectorBar bind:value={sel} bind:matched label="Select PCs by rule" id="nodes-sel">
		{#snippet actions()}
			{#if canAct}
				<button type="button" class="{btnGhost} self-start" disabled={!matched?.length} title={matched?.length ? '' : 'Type a selector that matches something'} onclick={() => (selected = [...matched!])}>
					Select {matched?.length ?? ''}
				</button>
			{/if}
		{/snippet}
	</SelectorBar>
</div>

{#if pending.length}
	<div class="border-warn bg-warn/10 mt-5 flex flex-wrap items-center justify-between gap-3 rounded-lg border px-4 py-3" role="status">
		<p class="text-[13px]"><span class="num font-semibold">{pending.length}</span> PC{pending.length === 1 ? ' is' : 's are'} waiting for approval. They get no tasks until approved.</p>
		{#if canAct}
			<button type="button" class={btnPrimary} disabled={busy} onclick={() => approve(pending.map((p) => p.node.node_id))}>
				<CircleCheck class="size-4" aria-hidden="true" />Approve all ({pending.length})
			</button>
		{:else}
			<p class="text-muted-foreground text-xs">An operator can approve them.</p>
		{/if}
	</div>
{/if}
{#if err}<p class="text-bad mt-3 text-[13px]" role="alert">{err}</p>{/if}
{#if selected.length}<div class="mt-4"><NodeActions ids={selected} ondone={() => (selected = [])} /></div>{/if}

<div class="mt-5">
	{#if !live.cluster}
		<div class="bg-surface border-line h-40 animate-pulse rounded-lg border" aria-busy="true"></div>
	{:else if all.length === 0}
		<EmptyState icon={Unplug} title="No PC has joined yet" body={canAct ? 'Create a join code, then run the command it shows on a lab PC.' : 'Start an agent on any PC on this network: ck agent.'} />
	{:else}
		<div class="bg-surface border-line relative overflow-x-auto rounded-lg border">
			<table class="w-full text-left text-[13px]">
				<thead class="text-muted-foreground border-line border-b text-xs">
					<tr>
						{#if canAct}
							<th class="w-9 px-3 py-2"><input type="checkbox" checked={allOn} aria-label="Select all shown" onchange={() => (selected = allOn ? [] : rows.map((r) => r.node.node_id))} /></th>
						{/if}
						<th class="px-3 py-2 font-medium">PC</th>
						<th class="px-3 py-2 font-medium">State</th>
						<th class="px-3 py-2 font-medium">Availability</th>
						<th class="px-3 py-2 font-medium">Seat</th>
						<th class="px-3 py-2 font-medium">Labels</th>
						<th class="px-3 py-2 font-medium">Pools</th>
						<th class="px-3 py-2 font-medium">Class</th>
						<th class="px-3 py-2 font-medium">CPU model</th>
						<th class="px-3 py-2 text-right font-medium">Threads</th>
						<th class="px-3 py-2 text-right font-medium">RAM</th>
						<th class="px-3 py-2 text-right font-medium">CPU</th>
						<th class="px-3 py-2 text-right font-medium">Rate</th>
						<th class="px-3 py-2 text-right font-medium">Chunks</th>
						{#if canAct}<th class="px-3 py-2"><span class="sr-only">Actions</span></th>{/if}
					</tr>
				</thead>
				<tbody>
					{#each rows as { node: n, w } (n.node_id)}
						<tr class="border-line border-b last:border-0 {n.state === 'pending' ? 'bg-warn/10' : ''} {n.online ? '' : 'text-muted-foreground'}">
							{#if canAct}
								<td class="px-3 py-2"><input type="checkbox" bind:group={selected} value={n.node_id} aria-label="Select {n.host}" /></td>
							{/if}
							<td class="px-3 py-2">
								<p class="font-medium">{n.host}</p>
								<p class="text-muted-foreground font-mono text-[11px]">{n.addr}{n.node_id ? ` · ${n.node_id}` : ''}{w?.virtual ? ' · virtual' : ''}</p>
							</td>
							<td class="px-3 py-2">
								<NodeStateChip state={n.state} online={n.online} power={n.power} id={n.node_id} />
								{#if n.state_reason}<p class="text-muted-foreground mt-0.5 max-w-48 truncate text-[11px]" title="{n.state_reason}{n.state_by ? ` (${n.state_by})` : ''}">{n.state_reason}{n.state_by ? ` · ${n.state_by}` : ''}</p>{/if}
							</td>
							<td class="px-3 py-2"><AvailChips presence={w?.presence ?? n.presence} blocked={!!w?.blocked} cap={w?.cap ?? null} /></td>
							<td class="px-3 py-2 whitespace-nowrap">{#if n.lab_id != null && n.x != null}{labName(n.lab_id) ?? 'Lab'} · {n.x + 1},{(n.y ?? 0) + 1}{:else}—{/if}</td>
							<td class="min-w-32 px-3 py-2"><LabelChips labels={n.labels} onremove={canAct ? (k) => void unlabel(n.node_id, k) : undefined} /></td>
							<td class="px-3 py-2">{#if n.pools.length}<span class="flex flex-wrap gap-1">{#each n.pools as p (p)}<span class="border-line rounded-md border px-1.5 py-0.5 text-[11px]">{p}</span>{/each}</span>{:else}<span class="text-muted-foreground">—</span>{/if}</td>
							<td class="text-muted-foreground max-w-40 truncate px-3 py-2 font-mono text-[11px]" title={n.class}>{n.class || '—'}</td>
							<td class="text-muted-foreground max-w-56 truncate px-3 py-2" title={n.inventory?.cpu_model}>{n.inventory?.cpu_model || '—'}</td>
							<td class="num px-3 py-2 text-right whitespace-nowrap">{count(w?.threads ?? n.inventory?.cores_logical)}</td>
							<td class="num px-3 py-2 text-right whitespace-nowrap">{bytes(n.inventory?.ram_bytes ?? w?.mem_total)}</td>
							<td class="num px-3 py-2 text-right whitespace-nowrap">{w ? pct(w.cpu) : '—'}</td>
							<td class="num px-3 py-2 text-right whitespace-nowrap">{rate(w?.rate)}</td>
							<td class="num px-3 py-2 text-right whitespace-nowrap">{count(w?.chunks)}</td>
							{#if canAct}
								<td class="px-3 py-2 text-right">
									{#if n.state === 'pending'}
										<button type="button" class="{btnPrimary} px-2.5 py-1" disabled={busy} onclick={() => approve([n.node_id])}>Approve</button>
									{/if}
								</td>
							{/if}
						</tr>
					{:else}
						<tr><td colspan="15" class="text-muted-foreground px-3 py-6 text-center">No PC matches{q ? ` “${q}”` : ''}{sel.trim() ? ' this selector' : ''}.</td></tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>
