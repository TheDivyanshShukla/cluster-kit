<script lang="ts">
	import { goto } from '$app/navigation';
	import { ArrowLeft, ChevronRight, CircleAlert, Gauge, Hash, List, Lock, Play, Save, Send, Terminal, Timer, Trash2, Upload } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import Listbox from '#lib/components/Listbox.svelte';
	import Segmented from '#lib/components/Segmented.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import { atLeast, emptyForm, validateJob } from '#lib/jobs.ts';
	import { PRIORITIES } from '#lib/api/schema.ts';
	import type { Estimate, Template } from '#lib/api/schema.ts';
	import { debounce, PRIORITY_LABEL } from '#lib/org.ts';
	import { estimateText, parseItems, PLACEMENTS, POLICIES, REDUCER_OPTIONS } from '#lib/sched.ts';
	import { live } from '#lib/live.svelte.ts';
	import { roleNow, userNow } from '#lib/session.svelte.ts';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { btnGhost, btnPrimary, card, field } from '#lib/ui.ts';

	const form = $state(emptyForm());
	const canSubmit = $derived(atLeast(roleNow(), 'researcher'));

	// templates (JOB-12): a saved job spec anyone can run again
	let templates = $state<Template[]>([]);
	let tplError = $state('');
	let tplName = $state('');
	let tplNote = $state('');
	let tplBusy = $state('');
	const loadTemplates = () => api.templates().then((r) => ((templates = r.templates), (tplError = ''))).catch((e) => (tplError = e instanceof ApiError ? e.message : 'Could not load templates.'));
	$effect(() => {
		void live.rev.templates;
		void loadTemplates();
	});
	const mine = (t: Template) => atLeast(roleNow(), 'admin') || t.owner === userNow();
	const specLine = (t: Template) => {
		const sp = t.spec as Record<string, unknown>;
		return [sp.kind, sp.n ? `n ${Number(sp.n).toLocaleString('en-IN')}` : null, sp.priority, sp.selector ? `on ${sp.selector}` : null].filter(Boolean).join(' · ');
	};
	async function saveTemplate() {
		failure = null;
		const v = validateJob(form);
		errors = v.errors;
		if (!v.value) return void (tplError = 'Fix the form first. A template stores the same job.');
		tplBusy = 'save';
		try {
			await api.saveTemplate(tplName.trim(), v.value, tplNote.trim());
			tplName = tplNote = '';
			await loadTemplates();
		} catch (e) {
			tplError = e instanceof ApiError ? e.message : 'Could not save the template.';
		}
		tplBusy = '';
	}
	async function runTemplate(t: Template) {
		tplBusy = t.name;
		tplError = '';
		try {
			await goto(`/jobs/${(await api.runTemplate(t.name)).job.id}`);
		} catch (e) {
			tplError = e instanceof ApiError ? e.message : 'Could not run the template.';
		}
		tplBusy = '';
	}
	async function removeTemplate(t: Template) {
		tplBusy = t.name;
		tplError = '';
		try {
			await api.deleteTemplate(t.name);
			await loadTemplates();
		} catch (e) {
			tplError = e instanceof ApiError ? e.message : 'Could not delete the template.';
		}
		tplBusy = '';
	}
	let errors = $state<Record<string, string>>({});
	let busy = $state(false);
	let failure = $state<string | null>(null);

	const canCmd = $derived(atLeast(roleNow(), 'operator'));
	const kinds = $derived([
		{ id: 'synth', label: 'Synthetic', hint: 'Fixed work per number. For tests.', icon: Timer, show: true },
		{ id: 'sha', label: 'SHA-256', hint: 'Find numbers that hash to a value.', icon: Hash, show: true },
		{ id: 'cmd', label: 'Command', hint: 'Run a shell command per range.', icon: Terminal, show: canCmd },
		{ id: 'list', label: 'List', hint: 'Run a command on each item you give.', icon: List, show: canCmd }
	] as const);

	const items = $derived(form.kind === 'list' ? parseItems(form.itemsText) : []);
	const nEff = $derived(form.kind === 'list' ? items.length : form.n);
	const policyHint = $derived(POLICIES.find((p) => p.value === form.policy)?.hint ?? '');
	const placeHint = $derived(PLACEMENTS.find((p) => p.value === form.placement)?.hint ?? '');
	const redHint = $derived(REDUCER_OPTIONS.find((o) => o.value === form.reducerId)?.hint ?? '');

	// "About 4 min on 23 PCs": asked after the person stops typing; stale answers are dropped.
	let est = $state<Estimate | null>(null);
	let estFailed = $state(false);
	let estSeq = 0;
	const askEstimate = debounce(async (n: number, kind: string, sel: string) => {
		const mine = ++estSeq;
		try {
			const r = await api.estimate(n, kind, sel.trim());
			if (mine === estSeq) (est = r), (estFailed = false);
		} catch {
			if (mine === estSeq) estFailed = true;
		}
	}, 400);
	$effect(() => {
		const n = nEff, kind = form.kind, sel = form.selector;
		if (!n || n < 1) {
			askEstimate.cancel();
			estSeq++;
			est = null;
			return;
		}
		est = null;
		estFailed = false;
		askEstimate(n, kind, sel);
		return () => askEstimate.cancel();
	});

	async function upload(e: Event) {
		const input = e.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		input.value = '';
		if (!file) return;
		if (file.size > 64 * 2 ** 20) return void (errors = { ...errors, items: 'That file is over 64 MiB.' });
		form.itemsText = await file.text();
		errors = { ...errors, items: '' };
	}

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		failure = null;
		const v = validateJob(form);
		errors = v.errors;
		if (!v.value) return;
		busy = true;
		try {
			const r = await api.createJob(v.value);
			await goto(`/jobs/${r.job.id}`);
		} catch (err) {
			failure = err instanceof ApiError ? err.message : 'Something went wrong. Try again.';
		}
		busy = false;
	}
	// Clearing one field's message as soon as the person edits it.
	const clear = (k: string) => () => {
		if (errors[k]) errors = { ...errors, [k]: '' };
	};
</script>

<Page
	title="New job"
	subtitle="Cover a range of numbers across every connected PC."
	tabs={[{ id: 'job', label: 'New job' }, { id: 'templates', label: 'Templates', badge: templates.length }]}
	class="lg:grid-cols-[minmax(0,1fr)_22rem]"
>
	{#snippet actions()}<a href="/jobs" class={btnGhost}><ArrowLeft class="size-4" aria-hidden="true" />All jobs</a>{/snippet}
{#if !atLeast(roleNow(), 'researcher')}
	<Panel title="New job" tab="job"><EmptyState icon={Lock} title="Your role cannot submit jobs" body="Ask an admin for the researcher role." /></Panel>
{:else}
	<Panel title="New job" tab="job">
	<form onsubmit={submit} novalidate class="max-w-2xl space-y-5">
		<fieldset>
			<legend class="mb-1.5 text-xs font-medium">Kind of work</legend>
			<div class="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
				{#each kinds.filter((k) => k.show) as k (k.id)}
					<label
						class="border-line has-[:checked]:border-primary has-[:checked]:bg-raised has-[:focus-visible]:outline-ring flex cursor-pointer gap-2.5 rounded-lg border p-3 has-[:focus-visible]:outline-2"
					>
						<input type="radio" name="kind" value={k.id} bind:group={form.kind} class="sr-only" />
						<k.icon class="text-muted-foreground mt-0.5 size-4 shrink-0" aria-hidden="true" />
						<span><span class="block font-medium">{k.label}</span><span class="text-muted-foreground block text-xs">{k.hint}</span></span>
					</label>
				{/each}
			</div>
			{#if !canCmd}<p class="text-muted-foreground mt-1.5 text-xs">Command jobs need the operator role.</p>{/if}
		</fieldset>

		{#if form.kind === 'synth'}
			<label class="block">
				<span class="mb-1 block text-xs font-medium">Work per number (ns)</span>
				<input class={field} type="number" inputmode="numeric" min="1" step="1" bind:value={form.synth_ns} oninput={clear('synth_ns')} aria-invalid={!!errors.synth_ns} aria-describedby="e-synth_ns" />
				<span id="e-synth_ns" class="text-bad mt-1 block text-xs">{errors.synth_ns ?? ''}</span>
			</label>
		{:else if form.kind === 'sha'}
			<label class="block">
				<span class="mb-1 block text-xs font-medium">SHA-256 to find (hex)</span>
				<input class="{field} font-mono" bind:value={form.sha} oninput={clear('sha')} autocomplete="off" spellcheck="false" maxlength="64" aria-invalid={!!errors.sha} aria-describedby="e-sha" />
				<span id="e-sha" class="text-bad mt-1 block text-xs">{errors.sha ?? ''}</span>
			</label>
		{:else if form.kind === 'list'}
			<label class="block">
				<span class="mb-1 block text-xs font-medium">Command (each item arrives on its standard input)</span>
				<input class="{field} font-mono" bind:value={form.cmd} oninput={clear('cmd')} autocomplete="off" spellcheck="false" aria-invalid={!!errors.cmd} aria-describedby="e-cmd" />
				<span id="e-cmd" class="text-bad mt-1 block text-xs">{errors.cmd ?? ''}</span>
			</label>
			<div>
				<div class="mb-1 flex flex-wrap items-center justify-between gap-2">
					<label for="items" class="text-xs font-medium">Items, one per line</label>
					<label class="{btnGhost} cursor-pointer px-2.5 py-1 text-xs has-[:focus-visible]:outline-2"><Upload class="size-3.5" aria-hidden="true" />Upload .txt<input type="file" accept=".txt,text/plain" class="sr-only" onchange={upload} /></label>
				</div>
				<textarea id="items" class="{field} h-36 font-mono" bind:value={form.itemsText} oninput={clear('items')} spellcheck="false" wrap="off" placeholder="one item per line" aria-invalid={!!errors.items} aria-describedby="e-items"></textarea>
				<p class="mt-1 flex justify-between gap-2 text-xs"><span id="e-items" class="text-bad">{errors.items ?? ''}</span><span class="text-muted-foreground num" aria-live="polite">{items.length.toLocaleString('en-IN')} item{items.length === 1 ? '' : 's'}</span></p>
			</div>
		{:else}
			<label class="block">
				<span class="mb-1 block text-xs font-medium">Command</span>
				<input class="{field} font-mono" bind:value={form.cmd} oninput={clear('cmd')} autocomplete="off" spellcheck="false" aria-invalid={!!errors.cmd} aria-describedby="e-cmd" />
				<span id="e-cmd" class="text-bad mt-1 block text-xs">{errors.cmd ?? ''}</span>
			</label>
		{/if}

		{#if form.kind !== 'list'}
		<div class="grid gap-4 sm:grid-cols-3">
			<label class="block">
				<span class="mb-1 block text-xs font-medium">Count (n)</span>
				<input class={field} type="number" inputmode="numeric" min="1" step="1" bind:value={form.n} oninput={clear('n')} aria-invalid={!!errors.n} aria-describedby="e-n" />
				<span id="e-n" class="text-bad mt-1 block text-xs">{errors.n ?? ''}</span>
			</label>
			<label class="block">
				<span class="mb-1 block text-xs font-medium">Start at (from)</span>
				<input class={field} type="number" inputmode="numeric" min="0" step="1" bind:value={form.from} oninput={clear('from')} aria-invalid={!!errors.from} aria-describedby="e-from" />
				<span id="e-from" class="text-bad mt-1 block text-xs">{errors.from ?? ''}</span>
			</label>
			<label class="block">
				<span class="mb-1 block text-xs font-medium">Chunk size</span>
				<input class={field} type="number" inputmode="numeric" min="0" step="1" bind:value={form.chunk} oninput={clear('chunk')} aria-invalid={!!errors.chunk} aria-describedby="e-chunk" />
				<span id="e-chunk" class="text-muted-foreground mt-1 block text-xs {errors.chunk ? 'text-bad' : ''}">{errors.chunk || '0 lets the head choose.'}</span>
			</label>
		</div>
		{/if}

		<SelectorBar bind:value={form.selector} label="Run on" id="job-sel" emptyMeans="Any PC" placeholder="lab=A pool=night" />
		<div>
			<p class="mb-1 text-xs font-medium" id="prio-l">Priority</p>
			<Segmented legend="Priority" options={PRIORITIES.map((p, i) => ({ value: p, label: PRIORITY_LABEL[i] }))} bind:value={form.priority} />
			<p class="text-muted-foreground mt-1 text-xs">Higher priority work gets PCs first when jobs compete.</p>
		</div>

		<div>
			<p class="mb-1 text-xs font-medium">Scheduling</p>
			<Segmented legend="Scheduling policy" options={POLICIES.map((p) => ({ value: p.value, label: p.label }))} bind:value={form.policy} />
			<p class="text-muted-foreground mt-1 text-xs">{policyHint}</p>
		</div>
		<div class="grid gap-4 sm:grid-cols-[16rem_7rem] sm:items-start">
			<div>
				<Listbox label="Combine results" id="reducer" options={REDUCER_OPTIONS} bind:value={form.reducerId} />
				<p class="text-muted-foreground mt-1 text-xs">{redHint}</p>
			</div>
			{#if form.reducerId === 'top'}
				<label class="block"><span class="mb-1 block text-xs font-medium">N</span><input class={field} type="number" min="1" step="1" inputmode="numeric" bind:value={form.reducerN} aria-invalid={!!errors.reducer} /><span class="text-bad mt-1 block text-xs">{errors.reducer ?? ''}</span></label>
			{/if}
		</div>

		<details class="border-line group rounded-lg border" open={!!(form.max_pcs || form.placement || form.mem_mb) || undefined}>
			<summary class="flex cursor-pointer items-center gap-1.5 px-3 py-2 text-[13px] font-medium"><ChevronRight class="size-4 transition-transform group-open:rotate-90" aria-hidden="true" />Advanced placement</summary>
			<div class="space-y-4 px-3 pb-3">
				<label class="block">
					<span class="mb-1 block text-xs font-medium">Use at most this many PCs (empty = all)</span>
					<input class={field} type="number" inputmode="numeric" min="1" step="1" placeholder="All PCs" bind:value={form.max_pcs} oninput={clear('max_pcs')} aria-invalid={!!errors.max_pcs} aria-describedby="e-max_pcs" />
					<span id="e-max_pcs" class="text-bad mt-1 block text-xs">{errors.max_pcs ?? ''}</span>
				</label>
				<div>
					<p class="mb-1 text-xs font-medium">Placement</p>
					<Segmented legend="Placement" options={PLACEMENTS.map((p) => ({ value: p.value, label: p.label }))} bind:value={form.placement} />
					<p class="text-muted-foreground mt-1 text-xs">{placeHint}</p>
				</div>
				<label class="block">
					<span class="mb-1 block text-xs font-medium">Memory per copy, MB (optional)</span>
					<input class={field} type="number" inputmode="numeric" min="1" step="1" placeholder="Not set" bind:value={form.mem_mb} oninput={clear('mem_mb')} aria-invalid={!!errors.mem_mb} aria-describedby="e-mem_mb" />
					<span id="e-mem_mb" class="text-muted-foreground mt-1 block text-xs {errors.mem_mb ? 'text-bad' : ''}">{errors.mem_mb || 'Each PC runs only as many copies as fit in its free memory.'}</span>
				</label>
			</div>
		</details>

		<label class="block">
			<span class="mb-1 block text-xs font-medium">Name (optional)</span>
			<input class={field} bind:value={form.name} maxlength="120" autocomplete="off" />
		</label>

		{#if nEff}
			<p class="bg-raised flex items-center gap-2 rounded-lg px-3 py-2 text-[13px]" role="status" aria-live="polite"><Gauge class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />{estimateText(est, estFailed)}</p>
		{/if}
		{#if errors.form}<p class="text-bad text-[13px]" role="alert">{errors.form}</p>{/if}
		{#if failure}
			<p class="text-bad flex items-start gap-1.5 text-[13px]" role="alert"><CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />{failure}</p>
		{/if}
		<div class="flex flex-wrap justify-end gap-2">
			<a href="/jobs" class={btnGhost}>Cancel</a>
			<button type="submit" class={btnPrimary} disabled={busy}><Send class="size-4" aria-hidden="true" />{busy ? 'Submitting…' : 'Submit job'}</button>
		</div>
	</form>
	</Panel>
{/if}
	<Panel title="Templates" tab="templates" count={templates.length}>
		{#if tplError}<p class="text-bad mb-3 flex items-start gap-1.5 text-[13px]" role="alert"><CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />{tplError}</p>{/if}
		{#if templates.length}
			<ul class="divide-line border-line divide-y rounded-lg border">
				{#each templates as t (t.name)}
					<li class="flex items-center gap-2 px-3 py-2">
						<div class="min-w-0 flex-1">
							<p class="truncate font-medium">{t.name}</p>
							<p class="text-muted-foreground text-xs break-words">{specLine(t)}</p>
							{#if t.note}<p class="text-muted-foreground text-xs break-words">{t.note}</p>{/if}
							<p class="text-muted-foreground text-xs">by {t.owner || 'unknown'}</p>
						</div>
						<button type="button" class="{btnGhost} h-8 px-2.5" disabled={!canSubmit || !!tplBusy} title={canSubmit ? `Submit a job from ${t.name}` : 'Running a template needs the researcher role'} aria-label="Run {t.name}" onclick={() => runTemplate(t)}><Play class="size-4" aria-hidden="true" />Run</button>
						<button type="button" class="{btnGhost} h-8 px-2.5" disabled={!mine(t) || !!tplBusy} title={mine(t) ? `Delete ${t.name}` : 'Only the owner or an admin can delete this template'} aria-label="Delete {t.name}" onclick={() => removeTemplate(t)}><Trash2 class="size-4" aria-hidden="true" /></button>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="text-muted-foreground py-4 text-center">No template yet. Fill in the form and save it below.</p>
		{/if}
		<div class="mt-4 space-y-2">
			<p class="text-xs font-medium">Save the form as a template</p>
			<input class={field} bind:value={tplName} placeholder="Name, for example nightly-sha" maxlength="40" autocomplete="off" aria-label="Template name" disabled={!canSubmit} />
			<input class={field} bind:value={tplNote} placeholder="Note (optional)" maxlength="200" autocomplete="off" aria-label="Template note" disabled={!canSubmit} />
			<button type="button" class="{btnGhost} w-full" disabled={!canSubmit || !tplName.trim() || !!tplBusy} title={canSubmit ? '' : 'Saving a template needs the researcher role'} onclick={saveTemplate}><Save class="size-4" aria-hidden="true" />Save template</button>
			{#if !canSubmit}<p class="text-muted-foreground text-xs">Saving and running templates needs the researcher role. You are signed in as {roleNow()}.</p>{/if}
		</div>
	</Panel>
</Page>
