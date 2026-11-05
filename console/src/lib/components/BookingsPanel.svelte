<script lang="ts">
	import { CalendarPlus, Plus, Trash2 } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { ReservationRow } from '#lib/api/schema.ts';
	import SelectorBar from '#lib/components/SelectorBar.svelte';
	import { datetime } from '#lib/format.ts';
	import { localDate, sortBookings, validateBooking } from '#lib/sched.ts';
	import { btnGhost, btnPrimary, card, field } from '#lib/ui.ts';

	let { canEdit }: { canEdit: boolean } = $props();
	let rows = $state<ReservationRow[] | null>(null);
	let error = $state('');
	let busy = $state(false);
	let errors = $state<Record<string, string>>({});
	const d0 = localDate(Date.now());
	const f = $state({ selector: '', startDate: d0, startTime: '09:00', endDate: d0, endTime: '17:00', note: '' });

	const load = () => api.reservations().then((r) => ((rows = sortBookings(r.reservations)), (error = ''))).catch((e) => (error = e instanceof ApiError ? e.message : 'Could not load bookings.'));
	$effect(() => {
		void load();
		const t = setInterval(load, 30_000);
		return () => clearInterval(t);
	});

	async function add(e: SubmitEvent) {
		e.preventDefault();
		const v = validateBooking(f, Date.now());
		errors = v.errors;
		if (!v.value) return;
		busy = true;
		try {
			await api.addReservation(v.value);
			f.note = '';
			error = '';
			await load();
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not add the booking.';
		}
		busy = false;
	}
	async function del(id: number) {
		try {
			await api.deleteReservation(id);
			await load();
		} catch (x) {
			error = x instanceof ApiError ? x.message : 'Could not delete the booking.';
		}
	}
</script>

<section aria-label="Bookings">
	<h2 class="sr-only">Bookings</h2>
	<p class="text-muted-foreground mt-0.5 text-[13px]">One-off times when matching PCs are kept free of jobs, such as an exam. Times are your local time.</p>
	{#if error}<p class="text-bad mt-2 text-[13px]" role="alert">{error}</p>{/if}
	<div class="{card} mt-3">
		{#if !rows}
			<div class="h-16 animate-pulse" aria-busy="true"></div>
		{:else if rows.length === 0}
			<p class="text-muted-foreground px-4 py-5 text-center">No bookings. Nothing is set aside right now.</p>
		{:else}
			<ul class="divide-line divide-y">
				{#each rows as r (r.id)}
					<li class="flex items-start justify-between gap-3 px-4 py-3">
						<div class="min-w-0">
							<p class="flex flex-wrap items-center gap-1.5 break-words">
								<CalendarPlus class="text-muted-foreground size-4 shrink-0" aria-hidden="true" /><span class="num">{datetime(r.start_ms)} to {datetime(r.end_ms)}</span>
								{#if r.active_now}<span class="text-bad bg-bad/10 rounded-full px-2 py-0.5 text-xs font-medium">Active now</span>{:else}<span class="text-muted-foreground bg-raised rounded-full px-2 py-0.5 text-xs">Upcoming</span>{/if}
							</p>
							<p class="text-muted-foreground mt-0.5 font-mono text-xs break-all">{r.selector || '(all PCs)'}</p>
							{#if r.note}<p class="text-muted-foreground mt-0.5 text-xs">{r.note}</p>{/if}
						</div>
						{#if canEdit}<button type="button" class="{btnGhost} px-2.5" aria-label="Delete booking {r.id}" onclick={() => void del(r.id)}><Trash2 class="size-4" aria-hidden="true" /></button>{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</div>
	{#if canEdit}
		<form onsubmit={add} novalidate class="{card} mt-3 space-y-3 p-4">
			<SelectorBar bind:value={f.selector} label="Which PCs" id="bk-sel" emptyMeans="Every PC" />
			<div class="grid gap-3 sm:grid-cols-2">
				<fieldset class="grid grid-cols-[1fr_6rem] gap-2">
					<legend class="mb-1 text-xs font-medium">Start (date YYYY-MM-DD, time HH:MM)</legend>
					<input class={field} bind:value={f.startDate} inputmode="numeric" maxlength="10" placeholder="2026-11-05" aria-label="Start date" aria-invalid={!!errors.start} autocomplete="off" />
					<input class={field} bind:value={f.startTime} inputmode="numeric" maxlength="5" placeholder="09:00" aria-label="Start time" aria-invalid={!!errors.start} autocomplete="off" />
					<span class="text-bad col-span-2 text-xs">{errors.start ?? ''}</span>
				</fieldset>
				<fieldset class="grid grid-cols-[1fr_6rem] gap-2">
					<legend class="mb-1 text-xs font-medium">End (date YYYY-MM-DD, time HH:MM)</legend>
					<input class={field} bind:value={f.endDate} inputmode="numeric" maxlength="10" placeholder="2026-11-05" aria-label="End date" aria-invalid={!!errors.end} autocomplete="off" />
					<input class={field} bind:value={f.endTime} inputmode="numeric" maxlength="5" placeholder="17:00" aria-label="End time" aria-invalid={!!errors.end} autocomplete="off" />
					<span class="text-bad col-span-2 text-xs">{errors.end ?? ''}</span>
				</fieldset>
			</div>
			<div class="grid gap-3 sm:grid-cols-[1fr_auto] sm:items-end">
				<label class="block"><span class="mb-1 block text-xs font-medium">Note (optional)</span><input class={field} bind:value={f.note} maxlength="200" placeholder="Midterm exam" /></label>
				<button type="submit" class={btnPrimary} disabled={busy}><Plus class="size-4" aria-hidden="true" />Add booking</button>
			</div>
		</form>
	{:else}
		<p class="text-muted-foreground mt-2 text-xs">Changing bookings needs the operator role.</p>
	{/if}
</section>
