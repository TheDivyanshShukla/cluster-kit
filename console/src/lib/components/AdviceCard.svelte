<script lang="ts">
	import { BellRing, Check, Eye, Info, Lightbulb, TriangleAlert } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { AdviceApplied, AdviceOption, AdvicePreviewReply, Suggestion } from '#lib/api/schema.ts';
	import { actionLine, changeLine, kindLabel, metricLine, SEV_LABEL } from '#lib/advisor.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import { ago, datetime } from '#lib/format.ts';
	import { btnGhost, btnPrimary } from '#lib/ui.ts';

	// One suggestion: why it showed up (the numbers), then every option side by side. Preview changes nothing; Apply asks first.
	interface Props {
		s: Suggestion;
		canPreview: boolean;
		canApply: boolean;
		now: number;
		/** the head's answer to apply/dismiss: the fresh list (and the record, on apply) */
		onresult: (list: Suggestion[], applied?: AdviceApplied) => void;
	}
	let { s, canPreview, canApply, now, onresult }: Props = $props();

	let previews = $state<Record<string, AdvicePreviewReply>>({});
	let errors = $state<Record<string, string>>({});
	let busy = $state('');
	let confirmFor = $state<AdviceOption | null>(null);
	let confirmOpen = $state(false);
	let snooze = $state('24');
	let note = $state('');
	$effect(() => {
		if (!confirmOpen) confirmFor = null;
	});

	const msg = (e: unknown, d: string) => (e instanceof ApiError ? e.message : d);
	const sev = $derived(({ warn: { icon: TriangleAlert, cls: 'text-warn bg-warn/10' }, tip: { icon: Lightbulb, cls: 'text-primary bg-primary/10' }, info: { icon: Info, cls: 'text-muted-foreground bg-raised' } })[s.severity]);
	const idBase = $derived(`adv-${s.id.replace(/[^a-z0-9]/gi, '_')}`);
	const RISK = { low: 'Low risk', med: 'Medium risk', high: 'High risk' } as const;

	async function preview(o: AdviceOption) {
		busy = `p:${o.id}`;
		errors[o.id] = '';
		try {
			previews[o.id] = await api.advicePreview(s.id, o.id);
		} catch (e) {
			errors[o.id] = msg(e, 'Could not preview this option.');
		}
		busy = '';
	}
	async function apply() {
		const o = confirmFor;
		if (!o) return;
		busy = `a:${o.id}`;
		errors[o.id] = '';
		try {
			const r = await api.adviceApply(s.id, o.id);
			confirmOpen = false;
			onresult(r.suggestions, r.applied);
		} catch (e) {
			confirmOpen = false;
			errors[o.id] = msg(e, 'Could not apply this option.');
		}
		busy = '';
	}
	async function dismiss() {
		busy = 'd';
		note = '';
		try {
			onresult((await api.adviceDismiss(s.id, Number(snooze))).suggestions);
		} catch (e) {
			note = msg(e, 'Could not snooze this suggestion.');
		}
		busy = '';
	}
	const row = 'grid grid-cols-[6.5rem_minmax(0,1fr)] gap-x-2 gap-y-0.5 py-1.5 sm:grid-cols-[7rem_minmax(0,1fr)]';
</script>

<article aria-labelledby="{idBase}-t" class="bg-surface border-line rounded-lg border">
	<header class="border-line flex flex-wrap items-center gap-x-3 gap-y-1.5 border-b px-4 py-3">
		<span class="inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium {sev.cls}"><sev.icon class="size-3.5 shrink-0" strokeWidth={2} aria-hidden="true" />{SEV_LABEL[s.severity]}</span>
		<h3 id="{idBase}-t" class="min-w-0 flex-1 text-[15px] font-semibold">{s.title}</h3>
		<span class="text-muted-foreground text-xs" title={datetime(s.since_ms)}>{kindLabel(s.kind)} · open {ago(s.since_ms, now)}{s.policy === 'auto' ? ' · auto' : ''}</span>
	</header>
	<div class="px-4 pt-3">
		<p class="text-muted-foreground text-xs font-medium tracking-wide uppercase">Why you see this</p>
		<p class="num mt-0.5 break-words">{s.why}</p>
		{#if s.accuracy != null && s.accuracy < 0.5}
			<p class="text-warn mt-2 flex items-start gap-1.5 text-[13px]"><TriangleAlert class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />Confidence lowered: only {Math.round(s.accuracy * 100)} % of past predictions for this rule held, so it will not run on its own.</p>
		{/if}
	</div>
	<ul class="grid gap-3 p-4 md:grid-cols-2 xl:grid-cols-3" aria-label="Options for {s.title}">
		{#each s.options as o (o.id)}
			{@const nothing = o.id === 'nothing'}
			{@const pv = previews[o.id]}
			<li class="flex min-w-0 flex-col rounded-lg border {o.recommended ? 'border-primary bg-primary/5' : 'border-line ' + (nothing ? 'bg-raised/50' : '')} p-3">
				<div class="flex flex-wrap items-start gap-x-2 gap-y-1">
					<h4 class="min-w-0 flex-1 text-[13px] font-semibold break-words">{o.title}</h4>
					{#if o.recommended}<span class="bg-primary text-primary-foreground inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium"><Check class="size-3" aria-hidden="true" />Recommended</span>{/if}
				</div>
				<dl class="divide-line mt-1 divide-y text-[13px]">
					<div class={row}><dt class="text-muted-foreground">{nothing ? 'If you wait' : 'Expected effect'}</dt><dd class="break-words">{o.expected_effect || 'No estimate.'}</dd></div>
					{#if !nothing}
						{#if o.metric}<div class={row}><dt class="text-muted-foreground">Measured by</dt><dd class="num">{metricLine(o.metric, o.metric_from, o.metric_to)}</dd></div>{/if}
						<div class={row}><dt class="text-muted-foreground">Cost</dt><dd class="break-words">{o.cost || 'None stated.'}</dd></div>
						<div class={row}>
							<dt class="text-muted-foreground">Risk</dt>
							<dd><span class={o.risk === 'low' ? '' : o.risk === 'med' ? 'text-warn font-medium' : 'text-bad font-medium'}>{RISK[o.risk]}</span> · {o.reversible ? 'can be undone' : 'cannot be undone'}{o.auto_safe ? ' · safe to run automatically' : ''}</dd>
						</div>
						<div class={row}><dt class="text-muted-foreground">Time to effect</dt><dd>{o.time_to_effect}</dd></div>
						<div class={row}><dt class="text-muted-foreground">Confidence</dt><dd class="break-words">{o.confidence}</dd></div>
						<div class={row}><dt class="text-muted-foreground">Success check</dt><dd class="break-words">{o.success_check}</dd></div>
					{/if}
				</dl>
				{#if o.steps.length}
					<div class="mt-2"><p class="text-muted-foreground text-xs font-medium">Steps for a person</p>
						<ol class="mt-1 list-decimal space-y-1 pl-5 text-[13px]">{#each o.steps as st, i (i)}<li class="break-words">{st}</li>{/each}</ol></div>
				{/if}
				{#if pv?.preview}
					<div class="bg-raised mt-2 rounded-md p-2 text-[13px]" role="status">
						<p class="flex items-center gap-1.5 font-medium"><Eye class="size-3.5" aria-hidden="true" />Preview: nothing has changed yet</p>
						{#if pv.preview.would_change.length}
							<ul class="num mt-1 space-y-0.5">{#each pv.preview.would_change as c, i (i)}<li class="break-words">{changeLine(c)}</li>{/each}</ul>
						{:else}<p class="text-muted-foreground mt-1">This option only has manual steps.</p>{/if}
						{#if !pv.preview.valid}<p class="text-bad mt-1" role="alert">The head would refuse this now{pv.preview.error ? `: ${pv.preview.error}` : '.'}</p>{/if}
					</div>
				{/if}
				{#if errors[o.id]}<p class="text-bad mt-2 text-[13px]" role="alert">{errors[o.id]}</p>{/if}
				{#if !nothing && o.action && o.action.type !== 'doc'}
					<div class="mt-auto flex flex-wrap gap-2 pt-3">
						{#if canPreview}
							<button type="button" class="{btnGhost} min-h-9" disabled={!!busy} onclick={() => preview(o)}><Eye class="size-3.5" aria-hidden="true" />{busy === `p:${o.id}` ? 'Checking…' : pv ? 'Preview again' : 'Preview'}</button>
						{/if}
						{#if canApply}
							<button type="button" class="{o.recommended ? btnPrimary : btnGhost} min-h-9" disabled={!!busy} onclick={() => ((confirmFor = o), (confirmOpen = true))}>Apply</button>
						{/if}
					</div>
				{:else if !nothing && !o.action}
					<p class="text-muted-foreground mt-auto pt-3 text-xs">The head cannot do this by itself.</p>
				{/if}
			</li>
		{/each}
	</ul>
	{#if canPreview}
		<footer class="border-line flex flex-wrap items-center gap-2 border-t px-4 py-2.5">
			<label class="text-muted-foreground flex items-center gap-2 text-[13px]">Snooze for
				<select bind:value={snooze} class="bg-surface border-line text-foreground rounded-lg border px-2 py-1.5 text-[16px] md:text-[13px]">
					<option value="1">1 hour</option><option value="24">1 day</option><option value="168">1 week</option>
				</select>
			</label>
			<button type="button" class="{btnGhost} min-h-9" disabled={!!busy} onclick={dismiss}><BellRing class="size-3.5" aria-hidden="true" />Snooze</button>
			{#if note}<p class="text-bad text-[13px]" role="alert">{note}</p>{/if}
		</footer>
	{/if}
</article>

<ConfirmDialog
	bind:open={confirmOpen}
	title="Apply: {confirmFor?.title ?? ''}"
	body="{actionLine(confirmFor?.action) || 'This change'} will be made now on the live cluster. {confirmFor?.reversible ? 'You can undo it afterwards.' : 'It cannot be undone.'}"
	confirm="Apply now"
	busy={busy.startsWith('a:')}
	onconfirm={apply}
/>
