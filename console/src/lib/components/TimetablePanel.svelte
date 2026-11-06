<script lang="ts">
	import { live } from '#lib/live.svelte.ts';
	import { CalendarClock, Plus, Trash2 } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { WindowRow } from '#lib/api/schema.ts';
	import Segmented from '#lib/components/Segmented.svelte';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import { DAY_LABEL, validateWindow, windowSegments, windowText } from '#lib/sched.ts';
	import { btnGhost, btnPrimary, card, field } from '#lib/ui.ts';

	let { canEdit }: { canEdit: boolean } = $props();
	let rows = $state<WindowRow[] | null>(null);
	let error = $state('');
	let busy = $state(false);
	let errors = $state<Record<string, string>>({});
	const f = $state({ selector: '', days: new Set<number>([0, 1, 2, 3, 4]), start: '09:00', end: '17:00', action: 'block' as 'block' | 'cap', cap: 2 as number | null, note: '' });

	const load = () => api.windows().then((r) => ((rows = r.windows), (error = ''))).catch((e) => (error = e instanceof ApiError ? e.message : 'Could not load the timetable.'));
	// active_now flips with the clock, so re-read every 30 s
	$effect(() => {
		void live.rev.timetable;
		void live.rev.windows;
		void load();
		const t = setInterval(load, 60_000);
		return () => clearInterval(t);
	});

	const HOURS = [0, 3, 6, 9, 12, 15, 18, 21];
	const pos = (m: number) => `${(m / 1440) * 100}%`;
	const blocks = (day: number) => (rows ?? []).flatMap((w) => windowSegments(w).filter((s) => s.day === day).map((s) => ({ w, s })));
	const today = $derived((new Date().getDay() + 6) % 7);

	function toggle(d: number) {
		const n = new Set(f.days);
		n.has(d) ? n.delete(d) : n.add(d);
		f.days = n;
		errors = { ...errors, days: '' };
	}
	async function add(e: SubmitEvent) {
		e.preventDefault();
		const v = validateWindow(f);
		errors = v.errors;
		if (!v.value) return;
		busy = true;
		try {
			await api.addWindow(v.value);
			f.note = '';
			error = '';
			await load();
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not add the window.';
		}
		busy = false;
	}
	async function del(id: number) {
		try {
			await api.deleteWindow(id);
			await load();
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not delete the window.';
		}
	}
</script>

<section>
	<h2 class="sr-only">Timetable</h2>
	<p class="text-muted-foreground mt-0.5 text-[13px]">Weekly windows when matching PCs take no jobs, or fewer threads. Times are the head's local time (India by default). A window that ends before it starts runs overnight.</p>
	{#if error}<p class="text-bad mt-2 text-[13px]" role="alert">{error}</p>{/if}

	<div class="{card} mt-3 p-3 sm:p-4">
		{#if !rows}
			<div class="h-32 animate-pulse" aria-busy="true"></div>
		{:else}
			<div class="grid grid-cols-[2.25rem_1fr] gap-x-2 gap-y-1" role="img" aria-label="Week grid: {rows.length} window{rows.length === 1 ? '' : 's'}">
				<span></span>
				<div class="text-muted-foreground relative h-4 text-[10px]">
					{#each HOURS as h (h)}<span class="num absolute -translate-x-1/2 first:translate-x-0" style="left:{pos(h * 60)}">{String(h).padStart(2, '0')}</span>{/each}
				</div>
				{#each DAY_LABEL as d, i (d)}
					<span class="text-xs {i === today ? 'font-semibold' : 'text-muted-foreground'} leading-7">{d}</span>
					<div class="bg-raised relative h-7 overflow-hidden rounded-md">
						{#each HOURS.slice(1) as h (h)}<span class="bg-line absolute inset-y-0 w-px" style="left:{pos(h * 60)}"></span>{/each}
						{#each blocks(i) as { w, s } (`${w.id}-${s.from}`)}
							<span
								class="absolute inset-y-0.5 overflow-hidden rounded px-1 text-[10px] leading-6 whitespace-nowrap {w.action === 'block' ? 'bg-bad/25 text-bad border-bad' : 'bg-warn/25 text-warn border-warn'} border {w.active_now ? 'ring-foreground ring-2' : ''}"
								style="left:{pos(s.from)};width:{pos(s.to - s.from)}"
								title="{w.selector || 'All PCs'}: {windowText(w)}{w.active_now ? ' (active now)' : ''}"
							>{w.action === 'block' ? 'Blocked' : `Cap ${w.cap_threads}`}</span>
						{/each}
					</div>
				{/each}
			</div>
			<p class="text-muted-foreground mt-2 flex flex-wrap gap-x-4 text-xs"><span><span class="bg-bad/25 border-bad inline-block size-2.5 rounded-sm border align-middle"></span> blocked</span><span><span class="bg-warn/25 border-warn inline-block size-2.5 rounded-sm border align-middle"></span> thread cap</span><span><span class="ring-foreground inline-block size-2.5 rounded-sm ring-2 align-middle"></span> active now</span></p>
		{/if}
	</div>

	{#if rows?.length}
		<ul class="{card} divide-line mt-3 divide-y">
			{#each rows as w (w.id)}
				<li class="flex items-start justify-between gap-3 px-4 py-3">
					<div class="min-w-0">
						<p class="flex flex-wrap items-center gap-1.5 break-words">
							<CalendarClock class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />{windowText(w)}
							{#if w.active_now}<span class="text-bad bg-bad/10 rounded-full px-2 py-0.5 text-xs font-medium">Active now</span>{/if}
						</p>
						<p class="text-muted-foreground mt-0.5 font-mono text-xs break-all">{w.selector || '(all PCs)'}</p>
						{#if w.note}<p class="text-muted-foreground mt-0.5 text-xs">{w.note}</p>{/if}
					</div>
					{#if canEdit}<button type="button" class="{btnGhost} px-2.5" aria-label="Delete window {w.id}" onclick={() => void del(w.id)}><Trash2 class="size-4" aria-hidden="true" /></button>{/if}
				</li>
			{/each}
		</ul>
	{:else if rows}
		<p class="text-muted-foreground {card} mt-3 px-4 py-5 text-center">No windows. Jobs may use PCs at any hour.</p>
	{/if}

	{#if canEdit}
		<form onsubmit={add} novalidate class="{card} mt-3 space-y-3 p-4">
			<SelectorBar bind:value={f.selector} label="Which PCs" id="win-sel" emptyMeans="Every PC" />
			<fieldset>
				<legend class="mb-1 text-xs font-medium">Days</legend>
				<div class="flex flex-wrap gap-1.5">
					{#each DAY_LABEL as d, i (d)}
						<button type="button" aria-pressed={f.days.has(i)} onclick={() => toggle(i)} class="border-line rounded-lg border px-3 py-1.5 text-[13px] {f.days.has(i) ? 'bg-primary text-primary-foreground border-primary' : 'bg-surface hover:bg-raised'}">{d}</button>
					{/each}
				</div>
				<span class="text-bad mt-1 block text-xs" role="alert">{errors.days ?? ''}</span>
			</fieldset>
			<div class="grid gap-3 sm:grid-cols-[7rem_7rem_auto_7rem_1fr] sm:items-start">
				<label class="block"><span class="mb-1 block text-xs font-medium">From (HH:MM)</span><input class={field} bind:value={f.start} inputmode="numeric" maxlength="5" placeholder="09:00" autocomplete="off" aria-invalid={!!errors.start} /><span class="text-bad mt-1 block text-xs">{errors.start ?? ''}</span></label>
				<label class="block"><span class="mb-1 block text-xs font-medium">To (HH:MM)</span><input class={field} bind:value={f.end} inputmode="numeric" maxlength="5" placeholder="17:00" autocomplete="off" aria-invalid={!!errors.end} /><span class="text-bad mt-1 block text-xs">{errors.end ?? ''}</span></label>
				<div><span class="mb-1 block text-xs font-medium">Then</span><Segmented legend="Action" bind:value={f.action} options={[{ value: 'block', label: 'Block jobs' }, { value: 'cap', label: 'Cap threads' }]} /></div>
				{#if f.action === 'cap'}
					<label class="block"><span class="mb-1 block text-xs font-medium">Threads</span><input class={field} type="number" min="1" step="1" inputmode="numeric" bind:value={f.cap} aria-invalid={!!errors.cap} /><span class="text-bad mt-1 block text-xs">{errors.cap ?? ''}</span></label>
				{:else}<span></span>{/if}
				<label class="block"><span class="mb-1 block text-xs font-medium">Note (optional)</span><input class={field} bind:value={f.note} maxlength="200" placeholder="Classes" /></label>
			</div>
			{#if errors.start || errors.end}<p class="text-muted-foreground text-xs">Example: 09:00 to 17:00 on Mon to Fri, or 22:00 to 06:00 to run overnight.</p>{/if}
			<div class="flex justify-end"><button type="submit" class={btnPrimary} disabled={busy}><Plus class="size-4" aria-hidden="true" />Add window</button></div>
		</form>
	{:else}
		<p class="text-muted-foreground mt-2 text-xs">Changing the timetable needs the operator role.</p>
	{/if}
</section>
