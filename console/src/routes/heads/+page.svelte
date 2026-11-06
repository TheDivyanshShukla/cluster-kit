<script lang="ts">
	import { CircleAlert, Crown, Eye, Hexagon, Lock, Network, Pause, Play, Server, Unlink } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { HierLab, HierMode, Hierarchy } from '#lib/api/schema.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import PinHeadDialog from '#lib/components/PinHeadDialog.svelte';
	import ProgressBar from '#lib/components/ProgressBar.svelte';
	import { count, rate } from '#lib/format.ts';
	import { diff, FIELDS, labBadge, lagText, lagTone, leaseProgress, MODES, takeoverIn, validate, type FieldKey, type LabTone } from '#lib/hier.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnGhost, btnPrimary, field } from '#lib/ui.ts';

	const role = $derived(roleNow());
	const canPin = $derived(atLeast(role, 'operator'));
	const canSet = $derived(atLeast(role, 'admin'));

	let loading = $state(true);
	let error = $state<string | null>(null);
	const h = $derived(live.hier);
	async function load() {
		try {
			live.hier = await api.hierarchy();
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not read the lab heads.';
		}
		loading = false;
	}
	$effect(() => {
		void load();
		const t = setInterval(load, 3000);
		return () => clearInterval(t);
	});
	const host = (id: string) => live.nodes[id]?.host ?? id;
	const tone: Record<LabTone, string> = { ok: 'text-ok bg-ok/10', warn: 'text-warn bg-warn/10', bad: 'text-bad bg-bad/10', idle: 'text-muted-foreground bg-raised' };
	const toneText: Record<LabTone, string> = { ok: 'text-ok', warn: 'text-warn', bad: 'text-bad', idle: 'text-muted-foreground' };

	// ---- writes
	let actErr = $state('');
	let busy = $state('');
	type Ask = { kind: 'mode'; mode: HierMode } | { kind: 'unpin' | 'drain' | 'undrain'; lab: string };
	let ask = $state<Ask | null>(null);
	let askOpen = $state(false);
	$effect(() => {
		if (!askOpen) ask = null;
	});
	const askText = $derived.by(() => {
		const a = ask;
		if (!a) return { title: '', body: '', confirm: '' };
		if (a.kind === 'mode') {
			const m = MODES.find((x) => x.id === a.mode)!;
			return {
				title: `Switch lab heads to ${m.label.toLowerCase()}`,
				body: `${m.what} This is live and safe in the middle of a job.`,
				confirm: `Switch to ${m.label.toLowerCase()}`
			};
		}
		const lab = a.lab;
		if (a.kind === 'unpin') return { title: `Unpin the lab head of ${lab}`, body: 'It stops being the lab head by choice. In auto mode the root picks again; in manual mode the lab goes back to working for the root.', confirm: 'Unpin' };
		if (a.kind === 'drain') return { title: `Drain ${lab}`, body: 'No new leases. When the ones it holds finish, the lab head sends its PCs back to the root and idles.', confirm: 'Drain' };
		return { title: `Resume ${lab}`, body: 'The lab head gets new leases again.', confirm: 'Resume' };
	});
	async function run(key: string, f: () => Promise<unknown>, fail: string) {
		busy = key;
		actErr = '';
		try {
			await f();
			await load();
		} catch (e) {
			actErr = e instanceof ApiError ? e.message : fail;
		}
		busy = '';
	}
	async function confirmAsk() {
		const a = ask;
		if (!a) return;
		if (a.kind === 'mode') await run('mode', async () => (live.hier = await api.setHierarchy({ mode: a.mode })), 'Could not switch the mode.');
		else if (a.kind === 'unpin') await run(`u:${a.lab}`, () => api.unpinLabHead(a.lab), 'Could not unpin.');
		else await run(`d:${a.lab}`, () => api.drainLab(a.lab, a.kind === 'drain'), 'Could not change the drain.');
		askOpen = false;
	}
	let pinLab = $state<HierLab | null>(null);
	let pinOpen = $state(false);
	$effect(() => {
		if (!pinOpen) pinLab = null;
	});
	const openPin = (l: HierLab) => ((pinLab = l), (pinOpen = true));

	// ---- settings form
	let vals = $state<Record<FieldKey, number>>({ lease_s: 30, lab_head_min_pcs: 8, lab_silence_s: 90 });
	let seeded = false;
	$effect(() => {
		const s = h?.settings;
		if (s && !seeded) {
			seeded = true;
			vals = { lease_s: s.lease_s, lab_head_min_pcs: s.lab_head_min_pcs, lab_silence_s: s.lab_silence_s };
		}
	});
	const errs = $derived(validate(vals));
	const patch = $derived(h ? diff(h.settings, vals) : {});
	const dirty = $derived(Object.keys(patch).length > 0);
	let setMsg = $state('');
	async function save() {
		await run('set', async () => {
			live.hier = await api.setHierarchy(patch);
			setMsg = 'Saved.';
		}, 'Could not save the settings.');
	}
	const modeLabel = (m: HierMode) => MODES.find((x) => x.id === m)!.label;
	const nLeased = (t: Hierarchy) => t.labs.filter((l) => l.head).length;
</script>

<Page
	title="Lab heads"
	subtitle="One head per lab serves that lab's PCs and talks to the root as if it were one big PC. The root then holds tens of connections instead of hundreds."
	always
	tabs={[{ id: 'topology', label: 'Topology', badge: h ? nLeased(h) : undefined }, { id: 'settings', label: 'Settings' }]}
>
	<Panel title="Topology" tab="topology" count={h ? `${modeLabel(h.mode)} · ${nLeased(h)} lab head${nLeased(h) === 1 ? '' : 's'}` : undefined} pad>
		{#if loading && !h}
			<div class="space-y-3" aria-busy="true" aria-label="Loading lab heads"><div class="bg-raised h-24 animate-pulse rounded-lg"></div><div class="bg-raised h-40 animate-pulse rounded-lg"></div></div>
		{:else if error && !h}
			<EmptyState icon={CircleAlert} title="Could not read the lab heads" body={error}>
				{#snippet action()}<button type="button" class={btnGhost} onclick={load}>Try again</button>{/snippet}
			</EmptyState>
		{:else if h}
			<div class="space-y-5">
				<!-- mode -->
				<section aria-labelledby="mode-h">
					<h2 id="mode-h" class="text-muted-foreground mb-2 text-xs font-semibold tracking-wide uppercase">Mode</h2>
					<div role="group" aria-label="Lab head mode" class="grid gap-2 sm:grid-cols-3">
						{#each MODES as m (m.id)}
							<button
								type="button"
								aria-pressed={h.mode === m.id}
								disabled={!canSet || busy === 'mode' || h.mode === m.id}
								onclick={() => ((ask = { kind: 'mode', mode: m.id }), (askOpen = true))}
								class="border-line min-h-11 rounded-lg border p-3 text-left disabled:cursor-default {h.mode === m.id ? 'border-primary bg-primary/5' : 'bg-surface hover:bg-raised disabled:opacity-60'}"
							>
								<span class="flex items-center gap-2 font-medium">{m.label}{#if h.mode === m.id}<span class="text-primary text-xs font-medium">current</span>{/if}</span>
								<span class="text-muted-foreground mt-0.5 block text-[13px]">{m.what}</span>
							</button>
						{/each}
					</div>
					{#if !canSet}<p class="text-muted-foreground mt-2 flex items-start gap-2 text-[13px]"><Lock class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />Only an admin can switch the mode{canPin ? '; you can pin, unpin and drain' : ''}.</p>{/if}
					{#if h.paused}
						<p class="text-warn mt-3 flex items-start gap-2 text-[13px]" role="status"><Pause class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />Paused: {h.paused}. Lab heads get no leases and their PCs work for the root until it ends.</p>
					{:else if h.mode !== 'off' && !h.active}
						<p class="text-muted-foreground mt-3 text-[13px]" role="status">On, but no lab head has a lease yet.</p>
					{/if}
					{#if h.unleasable_jobs.length}<p class="text-muted-foreground mt-2 text-[13px]">Job{h.unleasable_jobs.length === 1 ? '' : 's'} {h.unleasable_jobs.map((j) => `#${j}`).join(', ')} cannot be leased (command, list, SDK, selector or blobs), so the hierarchy pauses while {h.unleasable_jobs.length === 1 ? 'it has' : 'they have'} work.</p>{/if}
				</section>
				{#if actErr}<p class="text-bad text-[13px]" role="alert">{actErr}</p>{/if}

				<!-- tree -->
				<section aria-labelledby="tree-h">
					<h2 id="tree-h" class="sr-only">Root and labs</h2>
					<div class="border-line flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border p-3">
						<Crown class="text-primary size-5 shrink-0" aria-hidden="true" />
						<div class="min-w-0">
							<p class="font-medium">Root {host(h.root.node_id) || ''} <span class="text-muted-foreground text-xs font-normal">{h.root.role} · epoch {h.root.epoch}</span></p>
							<p class="text-muted-foreground num text-xs">{h.root.addr ?? ''}</p>
						</div>
						<p class="num ml-auto text-[13px]"><span class="font-medium">{count(h.root.pcs_direct)}</span> <span class="text-muted-foreground">PC{h.root.pcs_direct === 1 ? '' : 's'} direct</span></p>
					</div>
					{#if h.standbys.length}
						<ul class="mt-2 ml-3 flex flex-wrap gap-2 border-l-2 pl-3" aria-label="Standby heads" style="border-color: var(--line)">
							{#each h.standbys as s (s)}<li class="border-line text-muted-foreground inline-flex items-center gap-1.5 rounded-md border px-2 py-1 text-xs"><Eye class="size-3.5" aria-hidden="true" />standby {host(s)}</li>{/each}
						</ul>
					{/if}

					{#if h.mode === 'off' && nLeased(h) === 0}
						<div class="mt-4">
							<EmptyState icon={Network} title="Lab heads are off" body="Every PC works straight for the root. Turn on lab heads when the root gets busy with hundreds of PCs; each lab then gets its own head.">
								{#snippet action()}
									{#if canSet}<button type="button" class={btnPrimary} onclick={() => ((ask = { kind: 'mode', mode: 'manual' }), (askOpen = true))}>Turn on (manual)</button>
									{:else}<p class="text-muted-foreground text-[13px]">An admin can turn them on.</p>{/if}
								{/snippet}
							</EmptyState>
						</div>
					{:else if h.labs.length === 0}
						<div class="mt-4"><EmptyState icon={Server} title="No labs yet" body="Lab heads follow the labs of the floor map. Add a lab and place PCs in it first." /></div>
					{:else}
						<ul class="mt-2 ml-3 space-y-3 border-l-2 pl-3 sm:ml-5 sm:pl-5" aria-label="Labs" style="border-color: var(--line)">
							{#each h.labs as l (l.lab)}
								{@const hd = l.head}
								{@const b = labBadge(l)}
								{@const p = leaseProgress(hd)}
								{@const cut = b.text.startsWith('cut off')}
								{@const left = takeoverIn(h, l.lab)}
								<li class="border-line bg-surface rounded-lg border p-3 {cut ? 'border-warn/60' : ''}" aria-label="Lab {l.lab}">
									<div class="flex flex-wrap items-center gap-x-2 gap-y-1">
										<Hexagon class="size-4 shrink-0 {hd ? 'text-primary' : 'text-muted-foreground'}" aria-hidden="true" />
										<h3 class="font-semibold">{l.lab}</h3>
										<span class="rounded-full px-2 py-0.5 text-xs font-medium {tone[b.tone]}">{b.text}</span>
										{#if l.pinned}<span class="border-line text-muted-foreground rounded-full border px-2 py-0.5 text-xs">pinned to {host(l.pinned)}</span>{/if}
										{#if l.drain && !b.text.startsWith('draining')}<span class="text-warn bg-warn/10 rounded-full px-2 py-0.5 text-xs font-medium">draining</span>{/if}
									</div>
									{#if hd}
										<p class="text-muted-foreground num mt-1 text-[13px]">
											Lab head <span class="text-foreground font-medium">{hd.host || host(hd.node_id)}</span> · {count(hd.pcs)} PC{hd.pcs === 1 ? '' : 's'} under it · {count(hd.threads)} threads · {rate(hd.rate)}
											{#if l.pcs_at_root}· {count(l.pcs_at_root)} still at the root{/if}
										</p>
										<div class="mt-2.5 grid gap-x-6 gap-y-1.5 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
											<div>
												<ProgressBar value={p ?? 0} label="Lease progress for {l.lab}" tone={cut ? 'bg-warn' : 'bg-primary'} />
												<p class="num text-muted-foreground mt-1 text-xs">
													{#if p == null}No lease held right now.{:else}{count(hd.lab_done)} of {count(hd.lab_granted)} numbers done in the leases it holds · root counts {count(hd.granted_in_flight)} in flight{/if}
													· {count(hd.leases_done)} lease{hd.leases_done === 1 ? '' : 's'} finished
												</p>
											</div>
											<p class="num text-xs sm:text-right">
												<span class="text-muted-foreground">Last report</span> <span class="font-medium {toneText[lagTone(hd.lag_s, h.settings.lease_s)]}">{lagText(hd.lag_s)}</span>
												{#if hd.unacked}<span class="text-warn"> · {count(hd.unacked)} unacknowledged</span>{/if}
											</p>
										</div>
									{:else if l.state === 'absent'}
										<p class="text-muted-foreground mt-1 text-[13px]">{l.pinned ? `Waiting for ${host(l.pinned)} to start the lab head.` : 'Waiting for a PC to start the lab head.'}</p>
									{:else if l.pcs_at_root}
										<p class="text-muted-foreground mt-1 text-[13px]"><span class="num">{count(l.pcs_at_root)}</span> PC{l.pcs_at_root === 1 ? '' : 's'} work straight for the root.</p>
									{/if}
									{#if cut}
										<p class="text-warn mt-2 flex items-start gap-2 text-[13px]" role="status">
											<Unlink class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
											<span>Cut off from the root{l.silent_s != null ? ` for ${Math.round(l.silent_s)} s` : ''}. It keeps working on its lease{left != null ? `; the root takes ${l.orphaned_leases ? `its ${l.orphaned_leases} lease${l.orphaned_leases === 1 ? '' : 's'}` : 'them'} back in ${Math.round(left)} s if it stays silent` : ''}.</span>
										</p>
									{/if}
									{#if canPin}
										<div class="mt-3 flex flex-wrap gap-2">
											<button type="button" class="{btnGhost} min-h-9" disabled={h.mode === 'off' || !!busy} title={h.mode === 'off' ? 'Turn lab heads on first' : ''} onclick={() => openPin(l)}>{hd || l.pinned ? 'Move' : 'Pin a PC'}</button>
											{#if l.pinned}<button type="button" class="{btnGhost} min-h-9" disabled={!!busy} onclick={() => ((ask = { kind: 'unpin', lab: l.lab }), (askOpen = true))}>Unpin</button>{/if}
											{#if hd}
												<button type="button" class="{btnGhost} min-h-9" disabled={!!busy} onclick={() => ((ask = { kind: l.drain ? 'undrain' : 'drain', lab: l.lab }), (askOpen = true))}>
													{#if l.drain}<Play class="size-3.5" aria-hidden="true" />Resume{:else}<Pause class="size-3.5" aria-hidden="true" />Drain{/if}
												</button>
											{/if}
										</div>
									{/if}
								</li>
							{/each}
						</ul>
					{/if}
				</section>
			</div>
		{/if}
	</Panel>

	<Panel title="Settings" tab="settings" pad>
		<div class="max-w-2xl">
			{#if !h}
				<p class="text-muted-foreground text-[13px]">{error ?? 'Loading…'}</p>
			{:else}
				{#if !canSet}<p class="bg-raised text-muted-foreground mb-3 flex items-start gap-2 rounded-lg px-3 py-2 text-[13px]"><Lock class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />Only an admin can change these.</p>{/if}
				<form
					class="space-y-4"
					onsubmit={(e) => {
						e.preventDefault();
						setMsg = '';
						if (dirty && Object.keys(errs).length === 0) void save();
					}}
				>
					{#each FIELDS as f (f.key)}
						<div>
							<label for="hs-{f.key}" class="font-medium">{f.label}</label>
							<div class="mt-1 flex items-center gap-2">
								<input id="hs-{f.key}" type="number" inputmode="decimal" min={f.min} max={f.max} step={f.unit === 'PCs' ? 1 : 'any'} class="{field} max-w-36" bind:value={vals[f.key]} disabled={!canSet} aria-describedby="hs-{f.key}-d" aria-invalid={errs[f.key] ? true : undefined} />
								<span class="text-muted-foreground text-[13px]">{f.unit}</span>
							</div>
							<p id="hs-{f.key}-d" class="mt-1 text-xs {errs[f.key] ? 'text-bad' : 'text-muted-foreground'}" role={errs[f.key] ? 'alert' : undefined}>{errs[f.key] ?? `${f.help} ${f.min} to ${f.max} ${f.unit}.`}</p>
						</div>
					{/each}
					<div class="flex items-center gap-3">
						<button type="submit" class={btnPrimary} disabled={!canSet || !dirty || Object.keys(errs).length > 0 || busy === 'set'}>{busy === 'set' ? 'Saving…' : 'Save settings'}</button>
						{#if setMsg}<span class="text-ok text-[13px]" role="status">{setMsg}</span>{/if}
					</div>
					{#if actErr}<p class="text-bad text-[13px]" role="alert">{actErr}</p>{/if}
				</form>
			{/if}
		</div>
	</Panel>
</Page>

<ConfirmDialog bind:open={askOpen} title={askText.title} body={askText.body} confirm={askText.confirm} busy={!!busy} onconfirm={confirmAsk} />
<PinHeadDialog bind:open={pinOpen} lab={pinLab?.lab ?? ''} current={pinLab?.head?.node_id ?? pinLab?.pinned ?? null} ondone={load} />
