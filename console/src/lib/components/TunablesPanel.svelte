<script lang="ts">
	import { CircleAlert, Lock, RotateCcw, Search, SlidersHorizontal } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Tunable } from '#lib/api/schema.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { groupTunables, invalidKeys, isChanged, patchOf, pending, problem, rangeHint, readOnly, riskyOf, fmtNum } from '#lib/tunables.ts';
	import { btnGhost, btnPrimary, card, field } from '#lib/ui.ts';

	let list = $state<Tunable[] | null>(null);
	let failed = $state('');
	let q = $state('');
	let draft = $state<Record<string, string>>({});
	let busy = $state(false);
	let note = $state('');
	let ask = $state(false);
	const canEdit = $derived(atLeast(roleNow(), 'admin'));

	function load() {
		api.tunables().then((r) => ((list = r.tunables), (failed = ''))).catch((e) => (failed = e instanceof ApiError ? e.message : 'The head did not answer.'));
	}
	$effect(load);

	const groups = $derived(groupTunables(list ?? [], q));
	const todo = $derived(pending(list ?? [], draft));
	const bad = $derived(invalidKeys(list ?? [], draft));
	const risky = $derived(riskyOf(todo));
	const shownValue = (t: Tunable) => draft[t.key] ?? String(t.value);
	const edit = (t: Tunable, v: string) => (draft[t.key] = v);
	const reset = (t: Tunable) => (draft[t.key] = String(t.default));

	function save() {
		if (!todo.length || bad.length) return;
		if (risky.length && !ask) return void (ask = true);
		void send();
	}
	async function send() {
		busy = true;
		note = '';
		try {
			list = (await api.setTunables(patchOf(todo))).tunables;
			draft = {};
			note = 'Saved. Agents get the new values at once; a standby keeps them after a failover.';
		} catch (e) {
			note = e instanceof ApiError ? e.message : 'The head did not answer.';
		}
		busy = false;
		ask = false;
	}
</script>

<section class="{card} px-4 py-4" aria-label="All settings">
	<div class="flex flex-wrap items-center justify-between gap-2">
		<h2 class="flex items-center gap-2 font-medium"><SlidersHorizontal class="text-muted-foreground size-4" aria-hidden="true" />All settings</h2>
		<label class="relative block w-full sm:w-64">
			<span class="sr-only">Search settings</span>
			<Search class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2" aria-hidden="true" />
			<input type="search" bind:value={q} placeholder="Search, e.g. lost, standby, MB" class="{field} pl-9" />
		</label>
	</div>
	<p class="text-muted-foreground mt-1 text-[13px]">Every number the head and the PCs use. {canEdit ? 'Change what you need and press Save; the rest keeps its default.' : 'Changing these needs the admin role.'}</p>
	{#if list === null && !failed}
		<div class="bg-raised mt-3 h-48 animate-pulse rounded-lg" aria-busy="true"></div>
	{:else if failed && list === null}
		<div class="mt-3"><EmptyState icon={CircleAlert} title="Could not load the settings" body="{failed} Power and standby settings above still work." /></div>
	{:else if groups.length === 0}
		<p class="text-muted-foreground mt-4 text-[13px]">No setting matches "{q}". Try a shorter word.</p>
	{:else}
		{#each groups as g (g.group)}
			<h3 class="mt-5 text-[13px] font-semibold">{g.group}</h3>
			<ul class="divide-line mt-1 divide-y">
				{#each g.items as t (t.key)}
					{@const ro = readOnly(t) || !canEdit}
					{@const err = draft[t.key] !== undefined ? problem(t, draft[t.key]) : null}
					<li class="grid gap-x-4 gap-y-1.5 py-3 md:grid-cols-[minmax(0,1fr)_16rem]">
						<div class="min-w-0">
							<p class="flex flex-wrap items-center gap-x-2 gap-y-1">
								<code class="font-mono text-xs font-medium">{t.key}</code>
								{#if isChanged(t)}<span class="bg-warn/10 text-warn rounded-full px-2 py-0.5 text-xs font-medium">Changed</span>{/if}
								<span class="text-muted-foreground text-xs">{t.scope === 'head' ? 'head' : t.scope === 'agent' ? 'PCs' : 'head and PCs'}</span>
							</p>
							<p class="text-muted-foreground mt-0.5 text-[13px]">{t.help}</p>
							{#if readOnly(t)}<p class="text-muted-foreground mt-1 flex items-center gap-1.5 text-xs"><Lock class="size-3.5 shrink-0" aria-hidden="true" />Set by the environment variable CK_TUNE_{t.key.toUpperCase()} on the head. Change it there.</p>{/if}
						</div>
						<div>
							<div class="flex items-center gap-2">
								<input type="number" inputmode="decimal" step="any" min={t.min} max={t.max} value={shownValue(t)} oninput={(e) => edit(t, e.currentTarget.value)} disabled={ro} aria-label="{t.key} in {t.unit}" aria-invalid={!!err} class="{field} num min-w-0 flex-1 {err ? 'border-bad!' : ''} {draft[t.key] !== undefined && !err && Number(draft[t.key]) !== t.value ? 'border-primary!' : ''}" />
								<span class="text-muted-foreground w-14 shrink-0 truncate text-xs" title={t.unit}>{t.unit}</span>
								<button type="button" class="{btnGhost} size-9 shrink-0 px-0" aria-label="Reset {t.key} to {fmtNum(t.default)}" title="Reset to default ({fmtNum(t.default)})" disabled={ro || Number(shownValue(t)) === t.default} onclick={() => reset(t)}><RotateCcw class="size-4" aria-hidden="true" /></button>
							</div>
							<p class="mt-1 text-xs {err ? 'text-bad' : 'text-muted-foreground'}">{err ?? `${rangeHint(t)} · default ${fmtNum(t.default)}`}</p>
						</div>
					</li>
				{/each}
			</ul>
		{/each}
	{/if}
	{#if note}<p class="mt-3 text-[13px]" role="status">{note}</p>{/if}
</section>

{#if canEdit && (todo.length || bad.length)}
	<div class="bg-surface border-line sticky bottom-3 z-10 mt-3 flex flex-wrap items-center justify-between gap-3 rounded-lg border px-4 py-3 shadow-lg" role="region" aria-label="Unsaved changes">
		<p class="num text-[13px]">
			{#if bad.length}<span class="text-bad">{bad.length} value{bad.length === 1 ? '' : 's'} out of range.</span>{/if}
			{todo.length} change{todo.length === 1 ? '' : 's'} not saved{#if risky.length}<span class="text-warn"> · touches {risky.join(', ')}</span>{/if}
		</p>
		<span class="flex gap-2">
			<button type="button" class={btnGhost} onclick={() => (draft = {})} disabled={busy}>Discard</button>
			<button type="button" class={btnPrimary} onclick={save} disabled={busy || !todo.length || bad.length > 0}>{busy ? 'Saving…' : `Save ${todo.length} change${todo.length === 1 ? '' : 's'}`}</button>
		</span>
	</div>
{/if}

<ConfirmDialog
	bind:open={ask}
	title="Save {todo.length} change{todo.length === 1 ? '' : 's'}?"
	body="This touches {risky.join(', ')}. {risky.includes('Security') ? 'Sessions and login limits change for everyone. ' : ''}{risky.includes('Liveness') || risky.includes('Head failover') ? 'PCs or heads may be treated as lost sooner or later, and a failover can start or be delayed. ' : ''}Changed: {todo.map((x) => `${x.key} ${fmtNum(x.from)} to ${fmtNum(x.value)}`).join(', ')}."
	confirm="Save changes"
	{busy}
	onconfirm={send}
/>
