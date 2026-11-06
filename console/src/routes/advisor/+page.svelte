<script lang="ts">
	import { untrack } from 'svelte';
	import { CircleAlert, History, Lock, PartyPopper, Undo2 } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { AdviceApplied, AdviceMode, Suggestion } from '#lib/api/schema.ts';
	import { accuracyText, actionLine, adviceOrder, kindLabel, MODE_LABEL, metricLine, modeOf, policyKinds, SEV_LABEL, SEV_ORDER, verdict, VERDICT_LABEL } from '#lib/advisor.ts';
	import AdviceCard from '#lib/components/AdviceCard.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { ago, datetime } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnGhost } from '#lib/ui.ts';

	let list = $state<Suggestion[]>([]);
	let enabled = $state(true);
	let policy = $state<Record<string, string>>({});
	let accuracy = $state<Record<string, number>>({});
	let history = $state<AdviceApplied[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let histErr = $state<string | null>(null);
	let now = $state(Date.now());
	let recent = $state<AdviceApplied[]>([]); // applied in this visit, shown with their result and Undo
	let actErr = $state('');
	let setErr = $state('');
	let busy = $state('');
	let undoFor = $state<AdviceApplied | null>(null);
	let undoOpen = $state(false);
	$effect(() => {
		if (!undoOpen) undoFor = null;
	});

	const role = $derived(roleNow());
	const canPreview = $derived(atLeast(role, 'operator'));
	const canApply = $derived(atLeast(role, 'admin'));

	async function load() {
		try {
			const r = await api.advice();
			({ enabled, policy, accuracy } = r);
			list = r.suggestions;
			live.advice = r.suggestions;
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load suggestions.';
		}
		loading = false;
		now = Date.now();
	}
	async function loadHistory() {
		try {
			const r = await api.adviceHistory();
			history = r.history;
			accuracy = r.accuracy;
			histErr = null;
		} catch (e) {
			histErr = e instanceof ApiError ? e.message : 'Could not load the history.';
		}
	}
	$effect(() => {
		void load();
		void loadHistory();
		const t = setInterval(() => (now = Date.now()), 30_000);
		return () => clearInterval(t);
	});
	// Realtime: `advice` events replace the list.
	$effect(() => {
		const fresh = live.advice;
		if (fresh) untrack(() => (list = fresh));
	});

	const sorted = $derived([...list].sort(adviceOrder));
	const groups = $derived(SEV_ORDER.map((sev) => ({ sev, rows: sorted.filter((s) => s.severity === sev) })).filter((g) => g.rows.length));
	const openN = $derived(list.length);

	function onresult(l: Suggestion[], applied?: AdviceApplied) {
		list = l;
		live.advice = l;
		if (applied) {
			recent = [applied, ...recent];
			history = [applied, ...history.filter((h) => !(h.id === applied.id && h.ts_ms === applied.ts_ms))];
			void loadHistory();
		}
	}
	async function undo() {
		const a = undoFor;
		if (!a) return;
		busy = 'undo';
		actErr = '';
		try {
			onresult((await api.adviceUndo(a.id)).suggestions);
			recent = recent.filter((r) => r !== a);
			await loadHistory();
		} catch (e) {
			actErr = e instanceof ApiError ? e.message : 'Could not undo.';
		}
		undoOpen = false;
		busy = '';
	}

	async function save(b: Parameters<typeof api.setAdvice>[0], key: string) {
		busy = key;
		setErr = '';
		try {
			const r = await api.setAdvice(b);
			enabled = r.enabled;
			policy = r.policy;
			list = r.suggestions;
			live.advice = r.suggestions;
		} catch (e) {
			setErr = e instanceof ApiError ? e.message : 'Could not save the policy.';
		}
		busy = '';
	}
	const kinds = $derived(policyKinds(policy, [...list.map((s) => s.kind), ...history.map((h) => h.kind), ...Object.keys(accuracy)]));
	const undoable = (h: AdviceApplied) => canApply && !h.undone && h.action != null && h.action.type !== 'doc';
	const accRows = $derived(Object.entries(accuracy).sort((a, b) => a[0].localeCompare(b[0])));
	const num = (n: number | null | undefined) => (n == null ? 'n/a' : String(Math.round(n * 100) / 100));
</script>

<Page
	title="Advisor"
	subtitle="Measured problems with a fix to choose from. Nothing changes until an admin applies an option, unless you set a kind to Auto."
	always
	tabs={[
		{ id: 'suggestions', label: 'Suggestions', badge: openN || undefined },
		{ id: 'applied', label: 'Applied & outcomes', badge: history.length || undefined },
		{ id: 'policy', label: 'Policy' }
	]}
>
	<Panel title="Suggestions" tab="suggestions" count={openN ? `${openN} open` : 'none open'} pad>
		{#if loading}
			<div class="space-y-3" aria-busy="true" aria-label="Loading suggestions"><div class="bg-raised h-40 animate-pulse rounded-lg"></div><div class="bg-raised h-40 animate-pulse rounded-lg"></div></div>
		{:else if error && list.length === 0}
			<EmptyState icon={CircleAlert} title="Could not load suggestions" body={error}>
				{#snippet action()}<button type="button" class={btnGhost} onclick={load}>Try again</button>{/snippet}
			</EmptyState>
		{:else}
			{#if !canPreview}
				<p class="bg-raised text-muted-foreground mb-3 flex items-start gap-2 rounded-lg px-3 py-2 text-[13px]"><Lock class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />You can read suggestions. Operators can preview and snooze them; admins can apply them.</p>
			{:else if !canApply}
				<p class="bg-raised text-muted-foreground mb-3 flex items-start gap-2 rounded-lg px-3 py-2 text-[13px]"><Lock class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />You can preview and snooze. Only an admin can apply or undo a change.</p>
			{/if}
			{#if !enabled}
				<p class="text-warn mb-3 text-[13px]" role="status">The advisor is switched off (see Policy). Nothing new is suggested.</p>
			{/if}
			{#if actErr}<p class="text-bad mb-3 text-[13px]" role="alert">{actErr}</p>{/if}
			{#if recent.length}
				<ul class="mb-4 space-y-2" aria-label="Changes made in this visit">
					{#each recent as r (r.id + r.ts_ms)}
						<li class="border-ok/40 bg-ok/5 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-lg border px-3 py-2 text-[13px]" role="status">
							<span class="min-w-0 flex-1"><strong class="font-semibold">Applied</strong> · {kindLabel(r.kind)}: <span class="num">{actionLine(r.action) || r.option}</span>. {r.metric ? `The advisor checks ${metricLine(r.metric, r.before ?? 0, r.predicted ?? 0)} in ${Math.round(r.follow_s / 60)} min.` : ''}</span>
							{#if undoable(r)}<button type="button" class="{btnGhost} min-h-9" onclick={() => ((undoFor = r), (undoOpen = true))}><Undo2 class="size-3.5" aria-hidden="true" />Undo</button>{/if}
						</li>
					{/each}
				</ul>
			{/if}
			{#if list.length === 0}
				<EmptyState icon={PartyPopper} title="Nothing to improve right now" body="The advisor watches job timing, lost PCs, disk and logs. When something is worth fixing it appears here with the numbers behind it.">
					{#snippet action()}<a href="/alerts" class="text-primary text-[13px] font-medium hover:underline">See alerts</a>{/snippet}
				</EmptyState>
			{:else}
				<div class="space-y-6">
					{#each groups as g (g.sev)}
						<section aria-label={SEV_LABEL[g.sev]} class="space-y-3">
							<h3 class="text-muted-foreground text-xs font-semibold tracking-wide uppercase">{SEV_LABEL[g.sev]} · {g.rows.length}</h3>
							{#each g.rows as s (s.id)}<AdviceCard {s} {canPreview} {canApply} {now} {onresult} />{/each}
						</section>
					{/each}
				</div>
			{/if}
		{/if}
	</Panel>

	<Panel title="Applied & outcomes" tab="applied" count={history.length || undefined}>
		{#if accRows.length}
			<h3 class="text-muted-foreground mb-2 text-xs font-semibold tracking-wide uppercase">How often each rule was right</h3>
			<ul class="mb-5 flex flex-wrap gap-2">
				{#each accRows as [k, v] (k)}
					<li class="bg-raised rounded-lg px-3 py-1.5 text-[13px]"><span class="font-medium">{kindLabel(k)}</span> <span class="num {v < 0.5 ? 'text-warn' : 'text-ok'} font-medium">{Math.round(v * 100)} %</span></li>
				{/each}
			</ul>
		{/if}
		{#if histErr && history.length === 0}
			<EmptyState icon={CircleAlert} title="Could not load the history" body={histErr}>
				{#snippet action()}<button type="button" class={btnGhost} onclick={loadHistory}>Try again</button>{/snippet}
			</EmptyState>
		{:else if history.length === 0}
			<EmptyState icon={History} title="Nothing applied yet" body="When an option is applied, by an admin or by a kind set to Auto, it is listed here with what was predicted and what was measured 10 minutes later." />
		{:else}
			<ul class="divide-line divide-y" aria-label="Applied options">
				{#each history as h (h.id + h.ts_ms)}
					{@const v = verdict(h)}
					<li class="grid gap-x-4 gap-y-1 py-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]">
						<div class="min-w-0">
							<p class="font-medium">{kindLabel(h.kind)}{h.auto ? ' · automatic' : ''}</p>
							<p class="num text-muted-foreground break-words text-[13px]">{actionLine(h.action) || h.option}</p>
							<p class="text-muted-foreground text-xs" title={datetime(h.ts_ms)}>{h.auto ? 'by the advisor' : `by ${h.by}`} · {ago(h.ts_ms, now)}</p>
						</div>
						<div class="num min-w-0 text-[13px]">
							{#if h.metric}
								<p class="text-muted-foreground">{h.metric.replace(/_/g, ' ')}</p>
								<p>Before {num(h.before)} · predicted {num(h.predicted)} · observed {num(h.outcome?.observed)}</p>
								{#if h.outcome?.predicted_gain != null}<p class="text-muted-foreground text-xs">gain predicted {num(h.outcome.predicted_gain)}, observed {num(h.outcome.observed_gain)}</p>{/if}
							{:else}<p class="text-muted-foreground">No number to compare.</p>{/if}
						</div>
						<div class="flex flex-wrap items-start gap-2 sm:flex-col sm:items-end">
							<span class="rounded-full px-2 py-0.5 text-xs font-medium {v === 'held' ? 'text-ok bg-ok/10' : v === 'missed' ? 'text-bad bg-bad/10' : 'text-muted-foreground bg-raised'}">{VERDICT_LABEL[v]}</span>
							{#if undoable(h)}<button type="button" class="{btnGhost} min-h-9" onclick={() => ((undoFor = h), (undoOpen = true))}><Undo2 class="size-3.5" aria-hidden="true" />Undo</button>{/if}
						</div>
					</li>
				{/each}
			</ul>
		{/if}
		{#if actErr}<p class="text-bad mt-3 text-[13px]" role="alert">{actErr}</p>{/if}
	</Panel>

	<Panel title="Policy" tab="policy">
		<div class="max-w-3xl">
			<div class="border-line flex items-start gap-3 rounded-lg border p-3">
				<input id="adv-on" type="checkbox" role="switch" class="accent-primary mt-0.5 size-5 shrink-0" checked={enabled} disabled={!canApply || busy === 'on'} onchange={(e) => save({ enabled: e.currentTarget.checked }, 'on')} aria-describedby="adv-on-d" />
				<div>
					<label for="adv-on" class="font-medium">Advisor is {enabled ? 'on' : 'off'}</label>
					<p id="adv-on-d" class="text-muted-foreground text-[13px]">When off, nothing is suggested and nothing runs by itself.{canApply ? '' : ' Only an admin can change this.'}</p>
				</div>
			</div>
			{#if setErr}<p class="text-bad mt-3 text-[13px]" role="alert">{setErr}</p>{/if}
			<h3 class="mt-5 mb-1 text-[13px] font-semibold">What to do for each kind</h3>
			<p class="text-muted-foreground mb-2 text-[13px]"><strong>Suggest</strong> shows it here. <strong>Auto</strong> also applies the recommended option when it is reversible and inside safe bounds, at most once per 10 minutes, and can be undone. <strong>Off</strong> hides it.</p>
			<ul class="divide-line divide-y">
				{#each kinds as k (k.kind)}
					{@const acc = accuracy[k.kind]}
					{@const lowered = acc != null && acc < 0.5}
					{@const m = modeOf(policy, k.kind)}
					<li class="grid gap-x-4 gap-y-1 py-3 sm:grid-cols-[minmax(0,1fr)_9rem] sm:items-center">
						<div class="min-w-0">
							<label for="pol-{k.kind}" class="font-medium">{k.label}</label>
							<p class="text-muted-foreground text-xs" id="pol-{k.kind}-d">
								{#if !k.auto}Auto is not available: {k.why}
								{:else if lowered}Auto is paused: only {Math.round(acc * 100)} % of its predictions held.
								{:else if k.why}{k.why}
								{:else}Auto can apply a reversible change.{/if}
								{#if acc != null && !lowered} Accuracy: {accuracyText(acc)}.{/if}
							</p>
						</div>
						<select
							id="pol-{k.kind}"
							value={m}
							disabled={!canApply || busy === `p:${k.kind}`}
							aria-describedby="pol-{k.kind}-d"
							onchange={(e) => save({ policy: { [k.kind]: e.currentTarget.value as AdviceMode } }, `p:${k.kind}`)}
							class="bg-surface border-line text-foreground w-full rounded-lg border px-3 py-2 text-[16px] disabled:opacity-60 md:text-[13px]"
						>
							{#each ['off', 'suggest', 'auto'] as const as o (o)}
								<option value={o} disabled={o === 'auto' && (!k.auto || lowered) && m !== 'auto'}>{MODE_LABEL[o]}{o === 'auto' && (!k.auto || lowered) ? ' (not available)' : ''}</option>
							{/each}
						</select>
					</li>
				{/each}
			</ul>
		</div>
	</Panel>
</Page>

<ConfirmDialog
	bind:open={undoOpen}
	title="Undo this change"
	body="{actionLine(undoFor?.action)} goes back to the old value now."
	confirm="Undo now"
	busy={busy === 'undo'}
	onconfirm={undo}
/>
