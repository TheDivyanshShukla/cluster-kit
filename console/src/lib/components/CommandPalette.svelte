<script lang="ts">
	import { goto } from '$app/navigation';
	import { Building2, CalendarClock, Compass, Layers, ListChecks, Lock, Search, Server, ScrollText, SlidersHorizontal, User as UserIcon, Zap } from '@lucide/svelte';
	import type { Component } from 'svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { logSummary, type LogLine } from '#lib/api/logs.ts';
	import type { PoolRow, Tunable } from '#lib/api/schema.ts';
	import { railText } from '#lib/canvas/effects.ts';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import { atLeast } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { actionsFor, KIND_LABEL, PAGES, parseQuery, rank, type Act, type Entry, type Hit, type Kind } from '#lib/palette.ts';
	import { palette } from '#lib/palette.svelte.ts';
	import { roleNow } from '#lib/session.svelte.ts';

	// Ctrl/Cmd+K or "/": one box for PCs, jobs, labs, pools, users, settings, pages, events, log lines and actions.
	let dlg = $state<HTMLDialogElement>();
	let input = $state<HTMLInputElement>();
	let q = $state('');
	let cur = $state(0);
	let extra = $state<{ users: { name: string; role: string }[]; pools: PoolRow[]; tunables: Tunable[]; jobs: { id: number; name: string; state: string; submitter: string }[] }>({ users: [], pools: [], tunables: [], jobs: [] });
	let logs = $state<LogLine[]>([]);
	let sel = $state<{ q: string; count: number | null; err: string } | null>(null);
	let recent = $state<string[]>([]);
	let ask = $state<Act | null>(null);
	let askOpen = $state(false);
	let busy = $state(false);
	let err = $state('');

	const role = $derived(roleNow());
	const parsed = $derived(parseQuery(q));
	// closed palette: read nothing live, so a 5,000-PC fleet's every tick does not rebuild and rank 5,000 entries
	const ctx = $derived(palette.open ? { nodes: Object.values(live.nodes), labs: live.labs } : { nodes: [], labs: [] });
	const labName = (id: number | null) => live.labs.find((l) => l.id === id)?.name ?? 'no lab';

	const entries = $derived.by<Entry[]>(() => {
		const out: Entry[] = [];
		if (!palette.open) return out;
		for (const n of ctx.nodes) out.push({ id: `pc:${n.node_id}`, kind: 'pc', label: n.host, sub: `${labName(n.lab_id)} · ${n.online ? n.state : 'offline'}`, keys: `${n.node_id} ${n.class} ${Object.entries(n.labels).map(([k, v]) => `${k}=${v}`).join(' ')}`, href: `/live?sel=${encodeURIComponent(n.node_id)}` });
		const jobs = new Map(extra.jobs.map((j) => [j.id, j]));
		for (const j of Object.values(live.jobs)) jobs.set(j.id, j);
		for (const j of jobs.values()) out.push({ id: `job:${j.id}`, kind: 'job', label: `#${j.id} ${j.name}`.trim(), sub: `${j.state} · ${j.submitter}`, keys: String(j.id), href: `/jobs/${j.id}` });
		for (const l of live.labs) out.push({ id: `lab:${l.id}`, kind: 'lab', label: l.name, sub: `${ctx.nodes.filter((n) => n.lab_id === l.id).length} PCs`, href: `/live?filter=${encodeURIComponent(`lab=${l.name}`)}` });
		for (const p of extra.pools) out.push({ id: `pool:${p.name}`, kind: 'pool', label: p.name, sub: `${p.members.length} PCs${p.note ? ` · ${p.note}` : ''}`, href: '/policies' });
		for (const u of extra.users) out.push({ id: `user:${u.name}`, kind: 'user', label: u.name, sub: u.role, href: '/users' });
		for (const t of extra.tunables) out.push({ id: `set:${t.key}`, kind: 'set', label: t.key, sub: `${t.group} · ${t.help}`, keys: t.help, href: `/settings?t.q=${encodeURIComponent(t.key)}` });
		for (const p of PAGES) out.push({ id: `page:${p.href}`, kind: 'page', label: p.label, sub: p.href, keys: p.keys, href: p.href, need: p.need });
		for (const e of live.events.slice(0, 40)) out.push({ id: `ev:${e.id}`, kind: 'event', label: railText(e), sub: `${e.kind} · ${new Date(e.ts_ms).toLocaleTimeString('en-GB')}`, href: '/events' });
		logs.forEach((l, i) => out.push({ id: `log:${l.ts_ms}:${i}`, kind: 'log', label: logSummary(l) || l.kind, sub: `${l.level} · ${l.kind} · ${new Date(l.ts_ms).toLocaleTimeString('en-GB')}`, href: '/events?view=logs' }));
		if (!parsed.kind || parsed.kind === 'action') out.push(...actionsFor(parsed.text, ctx));
		return out;
	});
	const hits = $derived.by<Hit[]>(() => {
		const h = rank(entries, parsed, recent);
		return h;
	});
	// the selector offer comes first
	type Row = { type: 'sel'; key: string; label: string; sub: string; href: string } | { type: 'hit'; h: Hit };
	const rows = $derived.by<Row[]>(() => {
		const r: Row[] = [];
		if (parsed.selector) {
			const n = sel?.q === parsed.selector ? sel : null;
			const sub = n?.err ? n.err : n?.count != null ? `${n.count} PC${n.count === 1 ? '' : 's'} match` : 'checking…';
			r.push({ type: 'sel', key: 'sel:canvas', label: 'Show these on the canvas', sub, href: `/live?filter=${encodeURIComponent(parsed.selector)}` });
			r.push({ type: 'sel', key: 'sel:nodes', label: 'Show these in Nodes', sub, href: `/nodes?sel=${encodeURIComponent(parsed.selector)}` });
		}
		for (const h of hits) r.push({ type: 'hit', h });
		return r;
	});
	const kindOf = (r: Row): Kind | 'sel' => (r.type === 'sel' ? 'sel' : r.h.e.kind);
	const labelOf = (k: Kind | 'sel') => (k === 'sel' ? 'Selector' : KIND_LABEL[k]);
	const icons: Record<Kind | 'sel', Component> = { action: Zap, sel: Search, pc: Server, job: ListChecks, lab: Building2, pool: Layers, user: UserIcon, set: SlidersHorizontal, page: Compass, event: CalendarClock, log: ScrollText };
	const locked = (e: Entry) => !!e.need && !atLeast(role, e.need);
	const rowLocked = (r: Row) => r.type === 'hit' && locked(r.h.e);

	$effect(() => {
		if (!dlg) return;
		if (palette.open && !dlg.open) {
			q = '';
			cur = 0;
			err = '';
			try {
				recent = JSON.parse(localStorage.getItem('ck-palette-recent') ?? '[]');
			} catch {
				recent = [];
			}
			dlg.showModal();
			queueMicrotask(() => input?.focus());
			void loadExtra();
		}
		if (!palette.open && dlg.open) dlg.close();
	});
	let loaded = 0;
	async function loadExtra() {
		if (Date.now() - loaded < 30_000) return;
		loaded = Date.now();
		const [users, pools, tun, jobs] = await Promise.allSettled([atLeast(role, 'admin') ? api.users() : Promise.reject(), api.pools(), api.tunables(), api.jobs(100)]);
		extra = {
			users: users.status === 'fulfilled' ? users.value.users : [],
			pools: pools.status === 'fulfilled' ? pools.value.pools : [],
			tunables: tun.status === 'fulfilled' ? tun.value.tunables : [],
			jobs: jobs.status === 'fulfilled' ? jobs.value.jobs : []
		};
	}
	// log lines: debounced, and an older answer never overwrites a newer one
	let seq = 0;
	$effect(() => {
		const text = parsed.text, kind = parsed.kind;
		if (!palette.open || !atLeast(role, 'operator') || (kind && kind !== 'log') || text.length < 2 || parsed.selector) {
			logs = [];
			return;
		}
		const mine = ++seq;
		const t = setTimeout(async () => {
			try {
				const r = await api.logs({ limit: 300 });
				if (mine !== seq) return;
				const w = text.toLowerCase().split(/\s+/);
				logs = r.logs.filter((l) => w.every((x) => JSON.stringify(l).toLowerCase().includes(x))).slice(0, 20);
			} catch {
				if (mine === seq) logs = [];
			}
		}, 250);
		return () => clearTimeout(t);
	});
	// selector preview count (debounced)
	$effect(() => {
		const s = parsed.selector;
		if (!palette.open || !s) return;
		const t = setTimeout(async () => {
			try {
				const r = await api.select(s);
				sel = { q: s, count: r.count, err: r.error ?? '' };
			} catch (e) {
				sel = { q: s, count: null, err: e instanceof ApiError ? e.message : 'Could not check the rule.' };
			}
		}, 200);
		return () => clearTimeout(t);
	});
	$effect(() => {
		void q;
		cur = 0;
	});

	function remember(id: string) {
		recent = [id, ...recent.filter((x) => x !== id)].slice(0, 8);
		try {
			localStorage.setItem('ck-palette-recent', JSON.stringify(recent));
		} catch {}
	}
	function choose(r: Row | undefined) {
		if (!r || rowLocked(r)) return;
		if (r.type === 'sel') return void close(r.href);
		const e = r.h.e;
		remember(e.id);
		if (e.act) {
			ask = e.act;
			err = '';
			close();
			askOpen = true;
		} else if (e.href) close(e.href);
		else {
			q = '> ' + e.label.replace(/ …$/, '').toLowerCase() + ' ';
			input?.focus();
		}
	}
	function close(to?: string) {
		palette.open = false;
		if (to) void goto(to);
	}
	async function run() {
		const a = ask;
		if (!a) return;
		busy = true;
		err = '';
		try {
			const r = a.run;
			if (r.type === 'state') for (const n of (await api.setNodeState(r.ids, r.state)).nodes) live.nodes[n.node_id] = n;
			else if (r.type === 'power') for (const n of (await api.powerNodes(r.ids, r.action)).nodes) live.nodes[n.node_id] = n;
			else await api.switchover(r.id);
			askOpen = false;
		} catch (e) {
			err = e instanceof ApiError ? e.message : 'The action failed.';
		}
		busy = false;
	}
	function key(e: KeyboardEvent) {
		const n = rows.length;
		if (e.key === 'ArrowDown') (e.preventDefault(), (cur = n ? (cur + 1) % n : 0));
		else if (e.key === 'ArrowUp') (e.preventDefault(), (cur = n ? (cur - 1 + n) % n : 0));
		else if (e.key === 'Enter') (e.preventDefault(), choose(rows[cur]));
		else if (e.key === 'Tab' && n) {
			e.preventDefault();
			const kinds = rows.map(kindOf);
			const starts = kinds.map((k, i) => (i === 0 || kinds[i - 1] !== k ? i : -1)).filter((i) => i >= 0);
			const at = starts.filter((s) => s <= cur).length - 1;
			cur = starts[(at + (e.shiftKey ? -1 : 1) + starts.length) % starts.length];
		}
	}
	$effect(() => {
		void cur;
		dlg?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
	});

	// global keys: Ctrl/Cmd+K anywhere, "/" when the page has no search box of its own
	function global(e: KeyboardEvent) {
		if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
			e.preventDefault();
			palette.open = !palette.open;
			return;
		}
		const t = e.target as HTMLElement | null;
		if (e.key !== '/' || palette.open || e.defaultPrevented || e.ctrlKey || e.metaKey || t?.closest('input,textarea,select,[contenteditable],dialog')) return;
		const own = [...document.querySelectorAll<HTMLInputElement>('input[type=search]')].some((i) => i.offsetParent !== null);
		if (own) return;
		e.preventDefault();
		palette.open = true;
	}
</script>

<svelte:window onkeydown={global} />

<dialog
	bind:this={dlg}
	onclose={() => (palette.open = false)}
	onclick={(e) => e.target === dlg && close()}
	aria-label="Search and commands"
	class="bg-surface text-foreground border-line m-0 mx-auto mt-[8dvh] max-h-[80dvh] w-[min(40rem,calc(100vw-1.5rem))] overflow-hidden rounded-lg border p-0 shadow-2xl backdrop:bg-black/50"
>
	<div class="flex max-h-[80dvh] flex-col">
		<div class="border-line flex items-center gap-2 border-b px-3">
			<Search class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />
			<input
				bind:this={input}
				bind:value={q}
				onkeydown={key}
				role="combobox"
				aria-expanded="true"
				aria-controls="pal-list"
				aria-activedescendant={rows.length ? `pal-${cur}` : undefined}
				placeholder="Search PCs, jobs, settings… or type > for actions"
				autocomplete="off"
				spellcheck="false"
				maxlength="200"
				style="outline:0;box-shadow:none" class="h-12 min-w-0 flex-1 bg-transparent text-[16px] md:text-[14px]"
			/>
			<kbd class="text-muted-foreground border-line rounded border px-1.5 text-[11px] max-sm:hidden">Esc</kbd>
		</div>
		<div id="pal-list" role="listbox" aria-label="Results" class="thin min-h-0 flex-1 overflow-auto py-1">
			{#each rows as r, i (r.type === 'sel' ? r.key : r.h.e.id)}
				{@const k = kindOf(r)}
				{#if i === 0 || kindOf(rows[i - 1]) !== k}
					<p class="text-muted-foreground px-3 pt-2 pb-1 text-[11px] tracking-wide uppercase">{labelOf(k)}</p>
				{/if}
				{@const Icon = icons[k]}
				{@const off = rowLocked(r)}
				<div
					id="pal-{i}"
					role="option"
					tabindex="-1"
					aria-selected={cur === i}
					aria-disabled={off}
					class="flex min-h-11 cursor-pointer items-center gap-2.5 px-3 py-1.5 text-[13px] {cur === i ? 'bg-primary/10' : 'hover:bg-raised'} {off ? 'opacity-55' : ''}"
					onpointermove={() => (cur = i)}
					onclick={() => choose(r)}
					onkeydown={(e) => e.key === 'Enter' && choose(r)}
				>
					<Icon class="text-muted-foreground size-4 shrink-0" aria-hidden="true" />
					<span class="min-w-0 flex-1">
						<span class="block truncate font-medium">{r.type === 'sel' ? r.label : r.h.e.label}</span>
						{#if (r.type === 'sel' ? r.sub : r.h.e.sub)}<span class="text-muted-foreground block truncate text-xs">{r.type === 'sel' ? r.sub : r.h.e.sub}</span>{/if}
					</span>
					{#if off && r.type === 'hit'}<span class="text-muted-foreground inline-flex shrink-0 items-center gap-1 text-xs"><Lock class="size-3" aria-hidden="true" />needs {r.h.e.need}</span>{/if}
				</div>
			{:else}
				<p class="text-muted-foreground px-3 py-6 text-center text-[13px]">
					{q.trim() ? `Nothing matches "${parsed.text}".` : 'Start typing. Prefixes: pc: job: set: user: lab: pool: log: and > for actions.'}
				</p>
			{/each}
		</div>
		<div class="border-line text-muted-foreground flex items-center gap-3 border-t px-3 py-1.5 text-[11px] max-sm:hidden">
			<span><kbd>↑</kbd> <kbd>↓</kbd> move</span><span><kbd>Enter</kbd> open</span><span><kbd>Tab</kbd> next group</span><span class="ml-auto">Actions always ask first</span>
		</div>
	</div>
</dialog>

<ConfirmDialog bind:open={askOpen} title={ask?.title ?? ''} body={err || (ask?.body ?? '')} confirm={ask?.confirm ?? 'Confirm'} {busy} onconfirm={() => void run()} />
