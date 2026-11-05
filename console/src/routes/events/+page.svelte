<script lang="ts">
	import { CalendarClock, CircleAlert } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { EventRow } from '#lib/api/schema.ts';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import { live } from '#lib/live.svelte.ts';
	import { datetime } from '#lib/format.ts';
	import { mergeRows } from '#lib/jobs.ts';
	import { btnGhost, card, field } from '#lib/ui.ts';

	let items = $state<EventRow[]>([]);
	let next = $state<number | null>(null);
	let loading = $state(true);
	let more = $state(false);
	let error = $state<string | null>(null);
	let kind = $state('');

	async function load(before?: number | null) {
		try {
			const r = await api.events(50, before);
			items = before == null ? r.events : [...items, ...r.events];
			next = r.next;
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load events.';
		}
		loading = more = false;
	}
	$effect(() => void load());

	const all = $derived(mergeRows(items, live.events));
	const kinds = $derived([...new Set(all.map((e) => e.kind))].sort());
	const rows = $derived(kind ? all.filter((e) => e.kind === kind) : all);
</script>

<svelte:head><title>Events · Cluster Kit</title></svelte:head>

<div class="flex flex-wrap items-end justify-between gap-3">
	<div>
		<h1 class="text-xl font-semibold tracking-tight">Events</h1>
		<p class="text-muted-foreground mt-1">The audit log: who did what, and what the cluster did.</p>
	</div>
	<label class="w-full sm:w-56">
		<span class="mb-1 block text-xs font-medium">Kind</span>
		<select class="{field} appearance-none" bind:value={kind}>
			<option value="">All kinds</option>
			{#each kinds as k (k)}<option value={k}>{k}</option>{/each}
		</select>
	</label>
</div>

<div class="mt-5">
	{#if loading}
		<div class="{card} h-48 animate-pulse" aria-busy="true"></div>
	{:else if error && all.length === 0}
		<EmptyState icon={CircleAlert} title="Could not load events" body={error}>
			{#snippet action()}<button type="button" class={btnGhost} onclick={() => ((loading = true), load())}>Try again</button>{/snippet}
		</EmptyState>
	{:else if all.length === 0}
		<EmptyState icon={CalendarClock} title="Nothing logged yet" body="Logins, job changes and PCs joining are recorded here." />
	{:else}
		<div class="{card} relative overflow-x-auto">
			<table class="w-full text-left text-[13px]">
				<thead class="text-muted-foreground border-line border-b text-xs">
					<tr>
						<th class="px-3 py-2 font-medium">Time</th>
						<th class="px-3 py-2 font-medium">Kind</th>
						<th class="px-3 py-2 font-medium">Actor</th>
						<th class="px-3 py-2 font-medium">Detail</th>
					</tr>
				</thead>
				<tbody>
					{#each rows as e (e.id)}
						<tr class="border-line border-b align-top last:border-0">
							<td class="num text-muted-foreground px-3 py-2 whitespace-nowrap">{datetime(e.ts_ms)}</td>
							<td class="px-3 py-2 font-mono text-xs whitespace-nowrap">{e.kind}</td>
							<td class="px-3 py-2">{e.actor || '—'}</td>
							<td class="min-w-56 px-3 py-2 break-words">{e.detail}</td>
						</tr>
					{:else}
						<tr><td colspan="4" class="text-muted-foreground px-3 py-6 text-center">No loaded event has kind “{kind}”. Load older events to look further back.</td></tr>
					{/each}
				</tbody>
			</table>
		</div>
		{#if error}<p class="text-bad mt-3 text-[13px]" role="alert">{error}</p>{/if}
		{#if next != null}
			<div class="mt-4 flex justify-center">
				<button type="button" class={btnGhost} disabled={more} onclick={() => ((more = true), load(next))}>{more ? 'Loading…' : 'Load older events'}</button>
			</div>
		{/if}
	{/if}
</div>
