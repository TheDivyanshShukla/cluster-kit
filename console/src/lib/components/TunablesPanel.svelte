<script lang="ts">
	import { CircleAlert, Lock, RotateCcw, SlidersHorizontal } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Tunable } from '#lib/api/schema.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { Listing } from '#lib/listing.svelte.ts';
	import type { Spec } from '#lib/listing.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { invalidKeys, isChanged, patchOf, pending, problem, rangeHint, readOnly, riskyOf, fmtNum } from '#lib/tunables.ts';
	import { btnGhost, btnPrimary, field } from '#lib/ui.ts';

	let list = $state<Tunable[] | null>(null);
	let failed = $state('');
	let { tab = 'settings' }: { tab?: string } = $props();
	let draft = $state<Record<string, string>>({});
	let busy = $state(false);
	let note = $state('');
	let ask = $state(false);
	const canEdit = $derived(atLeast(roleNow(), 'admin'));

	function load() {
		api.tunables().then((r) => ((list = r.tunables), (failed = ''))).catch((e) => (failed = e instanceof ApiError ? e.message : 'The head did not answer.'));
	}
	$effect(load);

	const spec: Spec<Tunable> = {
		facets: [
			{ key: 'group', label: 'Group', get: (t) => t.group },
			{ key: 'source', label: 'Source', get: (t) => t.source, order: ['default', 'db', 'env'], text: (v) => (v === 'db' ? 'changed here' : v === 'env' ? 'environment' : v) },
			{ key: 'changed', label: 'Changed', get: (t) => (isChanged(t) ? 'yes' : 'no'), single: true, text: (v) => (v === 'yes' ? 'Changed only' : 'Default only') }
		],
		text: (t) => [t.key, t.key.replaceAll('_', ' '), t.help, t.group, t.unit],
		sorters: { key: (t) => t.key, changed: (t) => (isChanged(t) ? 0 : 1), group: (t) => t.group },
		base: () => 0 // the registry order groups related settings
	};
	const l = new Listing<Tunable>(spec, 't.');
	const res = $derived(l.run(list ?? []));
	// grouped in registry order unless a sort is chosen
	const groups = $derived(
		l.state.sort
			? [{ group: '', items: res.rows }]
			: res.rows.reduce<{ group: string; items: Tunable[] }[]>((a, t) => ((a.find((g) => g.group === t.group) ?? a[a.push({ group: t.group, items: [] }) - 1]!).items.push(t), a), [])
	);
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

<Panel title="All settings" {tab} count={list?.length} label="All settings">
	{#snippet toolbar()}
		<ListToolbar {l} rows={list ?? []} shown={res.rows.length} total={list?.length ?? 0} placeholder="Search, e.g. lost, standby, MB" sorts={[{ key: 'key', label: 'Name' }, { key: 'changed', label: 'Changed first' }]} />
	{/snippet}
	<p class="text-muted-foreground text-[13px]">Every number the head and the PCs use. {canEdit ? 'Change what you need and press Save; the rest keeps its default.' : 'Changing these needs the admin role.'}</p>
	{#if list === null && !failed}
		<div class="bg-raised mt-3 h-48 animate-pulse rounded-lg" aria-busy="true"></div>
	{:else if failed && list === null}
		<div class="mt-3"><EmptyState icon={CircleAlert} title="Could not load the settings" body="{failed} Power and standby settings still work." /></div>
	{:else if res.rows.length === 0}
		<div class="mt-3"><EmptyState icon={SlidersHorizontal} title="No setting matches these filters" body="Try a shorter word or clear the chips.">
			{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
		</EmptyState></div>
	{:else}
		{#each groups as g (g.group)}
			{#if g.group}<h3 class="mt-5 text-[13px] font-semibold first:mt-3">{g.group}</h3>{/if}
			<ul class="divide-line mt-1 divide-y">
				{#each g.items as t (t.key)}
					{@const ro = readOnly(t) || !canEdit}
					{@const err = draft[t.key] !== undefined ? problem(t, draft[t.key]) : null}
					<li class="grid gap-x-4 gap-y-1.5 py-3 md:grid-cols-[minmax(0,1fr)_16rem]">
						<div class="min-w-0">
							<p class="flex flex-wrap items-center gap-x-2 gap-y-1">
								<code class="font-mono text-xs font-medium"><Hl text={t.key} q={l.state.q} /></code>
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
							<p class="mt-1 text-xs {err ? 'text-bad' : 'text-muted-foreground'}" aria-live="polite">{err ?? `${rangeHint(t)} · default ${fmtNum(t.default)}`}</p>
						</div>
					</li>
				{/each}
			</ul>
		{/each}
	{/if}
	{#snippet footer()}
		{#if canEdit && (todo.length || bad.length)}
			<div class="flex flex-wrap items-center justify-between gap-3" role="region" aria-label="Unsaved changes">
				<p class="num text-[13px]">
					{#if bad.length}<span class="text-bad">{bad.length} value{bad.length === 1 ? '' : 's'} out of range.</span>{/if}
					{todo.length} change{todo.length === 1 ? '' : 's'} not saved{#if risky.length}<span class="text-warn"> · touches {risky.join(', ')}</span>{/if}
				</p>
				<span class="flex gap-2">
					<button type="button" class={btnGhost} onclick={() => (draft = {})} disabled={busy}>Discard</button>
					<button type="button" class={btnPrimary} onclick={save} disabled={busy || !todo.length || bad.length > 0}>{busy ? 'Saving…' : `Save ${todo.length} change${todo.length === 1 ? '' : 's'}`}</button>
				</span>
			</div>
		{:else if note}<p class="text-[13px]" role="status">{note}</p>{/if}
	{/snippet}
	{#if note && (todo.length || bad.length)}<p class="mt-3 text-[13px]" role="status">{note}</p>{/if}
</Panel>

<ConfirmDialog
	bind:open={ask}
	title="Save {todo.length} change{todo.length === 1 ? '' : 's'}?"
	body="This touches {risky.join(', ')}. {risky.includes('Security') ? 'Sessions and login limits change for everyone. ' : ''}{risky.includes('Liveness') || risky.includes('Head failover') ? 'PCs or heads may be treated as lost sooner or later, and a failover can start or be delayed. ' : ''}Changed: {todo.map((x) => `${x.key} ${fmtNum(x.from)} to ${fmtNum(x.value)}`).join(', ')}."
	confirm="Save changes"
	{busy}
	onconfirm={send}
/>
