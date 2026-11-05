<script lang="ts">
	import { page } from '$app/state';
	import { CircleAlert, Copy, Lock, ScrollText } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { LEVELS, logSummary, type LogLine } from '#lib/api/logs.ts';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { atLeast } from '#lib/jobs.ts';
	import { live } from '#lib/live.svelte.ts';
	import { Listing } from '#lib/listing.svelte.ts';
	import { KIND_GROUPS, kindGroup, rangeLabel, rangesOf, RANGES, type Spec } from '#lib/listing.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { setQuery } from '#lib/urlstate.svelte.ts';
	import { isPhone, vp } from '#lib/viewport.svelte.ts';
	import { btnGhost } from '#lib/ui.ts';

	// Structured logs from GET /api/v1/logs (docs/LOGGING.md). The head filters by prefix and level; the chips here
	// narrow what is loaded. The list re-reads every 4 s and shows "N new" when you are scrolled away from the top.
	type Row = { key: string; line: LogLine; text: string };
	const allowed = $derived(atLeast(roleNow(), 'operator'));
	let rows0 = $state<Row[]>([]);
	let limit = $state(Math.max(100, Math.min(20_000, Number(page.url.searchParams.get('n')) || 1000)));
	let error = $state<string | null>(null);
	let loading = $state(true);
	let focus = $state<string | null>(null);
	let now = $state(Date.now());
	let capped = $state(false);

	let fetched = $state<LogLine[]>([]);
	// SSE `log` lines newer than the fetched page are merged in; a 60 s refetch is the backup
	const merged = $derived.by(() => {
		void live.logRev;
		const newest = fetched.reduce((m, l) => Math.max(m, l.ts_ms), 0);
		const sig = (l: Record<string, unknown>) => JSON.stringify(l);
		const have = new Set(fetched.map(sig));
		const extra = (live.logTail as LogLine[]).filter((l) => l.ts_ms >= newest - 2000 && !have.has(sig(l)));
		return fetched.concat(extra.slice().reverse());
	});
	async function load() {
		try {
			const r = await api.logs({ since_ms: Date.now() - 24 * 3_600_000, limit });
			fetched = r.logs;
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load logs.';
		}
		loading = false;
	}
	$effect(() => {
		const seen = new Map<string, number>();
		{
			rows0 = merged
				.slice()
				.sort((a, b) => b.ts_ms - a.ts_ms)
				.map((line) => {
					const base = `${line.ts_ms}|${line.kind}|${line.node}`;
					const n = (seen.get(base) ?? 0) + 1;
					seen.set(base, n);
					return { key: `${base}|${n}`, line, text: logSummary(line) };
				});
			capped = fetched.length >= limit;
		}
		now = Date.now();
	});
	$effect(() => {
		if (!allowed) return;
		void limit;
		void load();
		const t = setInterval(load, 60_000);
		return () => clearInterval(t);
	});
	const more = () => capped && limit < 5000 && (limit = Math.min(5000, limit * 2));

	const spec: Spec<Row> = {
		facets: [
			{ key: 'group', label: 'Kind', get: (r) => kindGroup(r.line.kind), order: KIND_GROUPS },
			{ key: 'level', label: 'Level', get: (r) => r.line.level, order: [...LEVELS] },
			{ key: 'role', label: 'Role', get: (r) => r.line.role || '—' },
			{ key: 'range', label: 'When', get: (r) => rangesOf(r.line.ts_ms, now), single: true, order: RANGES.map((x) => x.key), text: rangeLabel }
		],
		text: (r) => [r.line.kind, r.line.node, r.line.role, r.text],
		sorters: { ts: (r) => r.line.ts_ms, level: (r) => LEVELS.indexOf(r.line.level), kind: (r) => r.line.kind, node: (r) => r.line.node },
		base: () => 0 // already newest first
	};
	const l = new Listing<Row>(spec);
	const res = $derived(l.run(rows0));
	const cur = $derived(rows0.find((r) => r.key === focus));
	const COLS: (Col & { min?: number })[] = [
		{ key: 'ts', label: 'Time', w: '112px', sort: true },
		{ key: 'level', label: 'Level', w: '64px', sort: true },
		{ key: 'kind', label: 'Kind', w: '150px', sort: true, min: 500 },
		{ key: 'node', label: 'Node', w: '70px', sort: true, min: 900 },
		{ key: 'text', label: 'Message', w: 'minmax(220px,1fr)' }
	];
	const cols = $derived<Col[]>(COLS.filter((c) => vp.w >= (c.min ?? 0)));
	const open = (r: Row) => ((focus = r.key), isPhone() && setQuery('tab', 'detail'));
	const clock = (ms: number) => {
		const d = new Date(ms);
		return `${d.toLocaleTimeString('en-GB')}.${String(d.getMilliseconds()).padStart(3, '0')}`;
	};
	const tone = { error: 'text-bad', warn: 'text-warn', info: 'text-foreground', debug: 'text-muted-foreground' } as const;
	const json = $derived(cur ? JSON.stringify(cur.line, null, 2) : '');
	let copied = $state(false);
	async function copy() {
		try {
			await navigator.clipboard.writeText(json);
			copied = true;
			setTimeout(() => (copied = false), 1500);
		} catch {}
	}
</script>

<Panel title="List" tab="list" label="Logs" count={rows0.length} scroll={false} pad={false}>
	{#snippet toolbar()}
		<ListToolbar {l} rows={rows0} shown={res.rows.length} total={rows0.length} placeholder="Search logs" />
	{/snippet}
	{#if !allowed}
		<div class="p-4"><EmptyState icon={Lock} title="Logs need the operator role" body="Ask an admin if you need to read the head's logs." /></div>
	{:else if loading}
		<div class="bg-raised m-3 h-40 animate-pulse rounded-lg" aria-busy="true"></div>
	{:else if error && rows0.length === 0}
		<div class="p-4">
			<EmptyState icon={CircleAlert} title="Could not load logs" body={error}>
				{#snippet action()}<button type="button" class={btnGhost} onclick={() => ((loading = true), load())}>Try again</button>{/snippet}
			</EmptyState>
		</div>
	{:else}
		<DataTable label="Logs" rows={res.rows} {cols} rowH={30} rowKey={(r) => r.key} sort={l.state.sort} dir={l.state.dir} onsort={(c) => l.sortBy(c)} active={(r) => r.key === focus} onrow={open} onend={more} pill={!l.state.sort}>
			{#snippet cell(r, c)}
				{#if c.key === 'ts'}<span class="num text-muted-foreground font-mono text-xs whitespace-nowrap">{clock(r.line.ts_ms)}</span>
				{:else if c.key === 'level'}<span class="font-mono text-xs font-medium {tone[r.line.level]}">{r.line.level}</span>
				{:else if c.key === 'kind'}<span class="block truncate font-mono text-xs"><Hl text={r.line.kind} q={l.state.q} /></span>
				{:else if c.key === 'node'}<span class="text-muted-foreground block truncate font-mono text-xs">{r.line.node || r.line.role}</span>
				{:else}<span class="block truncate font-mono text-xs" title={r.text}><Hl text={r.text} q={l.state.q} /></span>
				{/if}
			{/snippet}
			{#snippet empty()}
				<div class="p-4">
					{#if rows0.length === 0}
						<EmptyState icon={ScrollText} title="No log lines in the last 24 hours" body="Lines appear here as the head, agents and standbys write them." />
					{:else}
						<EmptyState icon={ScrollText} title="No log line matches these filters" body="Nothing fits the search and chips you chose.">
							{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
						</EmptyState>
					{/if}
				</div>
			{/snippet}
		</DataTable>
	{/if}
</Panel>

<Panel title="Detail" tab="detail" label="Selected log line">
	{#snippet actions()}
		{#if cur}<button type="button" class="{btnGhost} h-7 px-2 text-xs" onclick={copy}><Copy class="size-3.5" aria-hidden="true" />{copied ? 'Copied' : 'Copy JSON'}</button>{/if}
	{/snippet}
	{#if cur}
		<pre class="font-mono text-xs break-all whitespace-pre-wrap">{json}</pre>
	{:else}
		<p class="text-muted-foreground text-[13px]">Click a line to see its full JSON here.</p>
	{/if}
</Panel>
