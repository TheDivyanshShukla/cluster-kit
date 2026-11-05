<script lang="ts">
	import { CircleAlert, Crown, Eye, UserPlus } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Candidate } from '#lib/api/schema.ts';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import { live } from '#lib/live.svelte.ts';

	// Who the head would pick next: score = weighted speed, memory, stability and free (weights are under All settings).
	let rows = $state<Candidate[] | null>(null);
	let failed = $state('');
	const roleMeta = { head: { icon: Crown, label: 'Head', cls: 'text-primary bg-primary/10' }, standby: { icon: Eye, label: 'Standby', cls: 'text-ok bg-ok/10' }, candidate: { icon: UserPlus, label: 'Candidate', cls: 'text-muted-foreground bg-raised' } } as const;
	const parts = ['speed', 'memory', 'stability', 'free'] as const;
	const pct = (x: number) => Math.round(x * 100);

	// refresh when the head or the standby list changes, and now and then (stability moves)
	const stamp = $derived(`${live.ha?.epoch}:${live.ha?.standbys.map((s) => s.node_id).join(',')}`);
	$effect(() => {
		void stamp;
		let live_ = true;
		const load = () => api.candidates().then((r) => live_ && ((rows = r.candidates), (failed = ''))).catch((e) => live_ && (failed = e instanceof ApiError ? e.message : 'The head did not answer.'));
		void load();
		const t = setInterval(load, 30_000);
		return () => { live_ = false; clearInterval(t); };
	});
	const shown = $derived((rows ?? []).slice(0, 12));
</script>

<section aria-label="Head candidates">
	<h3 class="text-[13px] font-medium">Who would be picked next</h3>
	<p class="text-muted-foreground mt-1 text-[13px]">Best first. The head recruits the best PC that can be a standby, preferring a lab that has none.</p>
	{#if rows === null && !failed}
		<div class="bg-raised mt-3 h-40 animate-pulse rounded-lg" aria-busy="true"></div>
	{:else if failed && rows === null}
		<div class="mt-3"><EmptyState icon={CircleAlert} title="Could not load the ranking" body="{failed} The standby list above still works." /></div>
	{:else if shown.length === 0}
		<div class="mt-3"><EmptyState icon={UserPlus} title="No PC to rank yet" body="Once PCs join, they are scored here." /></div>
	{:else}
		<div class="border-line mt-3 overflow-x-auto rounded-lg border">
			<table class="w-full min-w-[34rem] text-left text-[13px]">
				<caption class="sr-only">PCs ranked by head score</caption>
				<thead class="text-muted-foreground border-line border-b text-xs">
					<tr>
						<th class="px-3 py-2 font-medium">PC</th><th class="px-3 py-2 font-medium">Score</th>
						{#each parts as p (p)}<th class="px-2 py-2 text-right font-medium capitalize">{p}</th>{/each}
						<th class="px-3 py-2 font-medium">Role</th><th class="px-3 py-2 font-medium">Why not</th>
					</tr>
				</thead>
				<tbody>
					{#each shown as c (c.node_id)}
						{@const m = roleMeta[c.role]}
						<tr class="border-line border-b last:border-0 {c.eligible || c.role !== 'candidate' ? '' : 'text-muted-foreground'}">
							<th scope="row" class="max-w-[9rem] px-3 py-2 font-medium"><span class="block truncate" title={c.host}>{c.host}</span>{#if c.lab}<span class="text-muted-foreground block truncate text-xs font-normal">{c.lab}</span>{/if}</th>
							<td class="px-3 py-2">
								<span class="flex items-center gap-2">
									<span class="bg-raised h-2 w-20 shrink-0 rounded-full" role="img" aria-label="Score {c.score} of 100"><span class="bg-primary block h-full rounded-full" style="width:{Math.max(0, Math.min(100, c.score))}%"></span></span>
									<span class="num w-9 text-right font-semibold">{Math.round(c.score)}</span>
								</span>
							</td>
							{#each parts as p (p)}<td class="num px-2 py-2 text-right">{pct(c.parts[p])}</td>{/each}
							<td class="px-3 py-2"><span class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium {m.cls}"><m.icon class="size-3.5" aria-hidden="true" />{m.label}</span></td>
							<td class="px-3 py-2 text-xs">{c.why || (c.role === 'candidate' ? 'can be a standby' : '')}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		{#if (rows?.length ?? 0) > shown.length}<p class="text-muted-foreground mt-1 text-xs">Top {shown.length} of {rows?.length}.</p>{/if}
	{/if}
</section>
