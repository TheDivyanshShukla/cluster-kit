<script lang="ts">
	import { untrack } from 'svelte';
	import { BellOff, Check, CircleAlert } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Alert } from '#lib/api/schema.ts';
	import { alertOrder, KIND_LABEL, mergeAlerts, SEV_LABEL, SEVERITIES } from '#lib/alerts.ts';
	import AlertChip from '#lib/components/AlertChip.svelte';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { ago, datetime, duration } from '#lib/format.ts';
	import { Listing } from '#lib/listing.svelte.ts';
	import type { Spec } from '#lib/listing.ts';
	import { live } from '#lib/live.svelte.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { vp } from '#lib/viewport.svelte.ts';
	import { btnGhost } from '#lib/ui.ts';

	let rows0 = $state<Alert[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let ackErr = $state('');
	let tick = $state(Date.now());

	async function load() {
		try {
			rows0 = mergeAlerts(rows0, (await api.alerts()).alerts);
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load alerts.';
		}
		loading = false;
		tick = Date.now();
	}
	// Realtime: `alert` events on the stream land in live.alerts (and are re-read by the store on reconnect).
	$effect(() => {
		const fresh = live.alerts;
		untrack(() => (rows0 = mergeAlerts(rows0, fresh)));
	});
	$effect(() => void load());

	const host = (a: Alert) => (a.node_id ? (live.nodes[a.node_id]?.host ?? a.node_id) : '');
	const status = (a: Alert) => (a.active ? 'Active' : 'Closed');
	const spec: Spec<Alert> = {
		facets: [
			{ key: 'status', label: 'Status', get: status, order: ['Active', 'Closed'] },
			{ key: 'kind', label: 'Kind', get: (a) => a.kind, text: (v) => KIND_LABEL[v as Alert['kind']] ?? v },
			{ key: 'severity', label: 'Severity', get: (a) => a.severity, order: [...SEVERITIES], text: (v) => SEV_LABEL[v as Alert['severity']] ?? v }
		],
		text: (a) => [KIND_LABEL[a.kind], a.kind, host(a), a.detail, a.acked_by, a.severity],
		sorters: {
			severity: (a) => SEVERITIES.indexOf(a.severity),
			kind: (a) => KIND_LABEL[a.kind],
			node: (a) => host(a),
			since: (a) => a.since_ms,
			status: (a) => Number(!a.active)
		},
		base: alertOrder
	};
	const l = new Listing<Alert>(spec);
	const res = $derived(l.run(rows0));
	const activeN = $derived(rows0.filter((a) => a.active).length);
	const canAck = $derived(atLeast(roleNow(), 'operator'));

	const COLS: (Col & { min?: number })[] = [
		{ key: 'severity', label: 'Severity', w: '104px', sort: true },
		{ key: 'kind', label: 'Alert', w: 'minmax(120px,1fr)', sort: true },
		{ key: 'node', label: 'PC', w: '84px', sort: true, min: 700 },
		{ key: 'detail', label: 'What happened', w: 'minmax(160px,3fr)', min: 1000 },
		{ key: 'since', label: 'Opened', w: '120px', sort: true },
		{ key: 'status', label: 'Status', w: '150px', sort: true, min: 700 },
		{ key: 'ack', label: '', w: '88px', min: 700 }
	];
	const cols = $derived<Col[]>(COLS.filter((c) => vp.w >= (c.min ?? 0) && (c.key !== 'ack' || canAck)));

	async function ack(a: Alert) {
		ackErr = '';
		try {
			rows0 = mergeAlerts(rows0, [(await api.ackAlert(a.id)).alert]);
		} catch (e) {
			ackErr = e instanceof ApiError ? e.message : 'Could not acknowledge the alert.';
		}
	}
</script>

<Page title="Alerts" subtitle="Slow or flaky PCs, a busy head, a lagging standby, a reserve about to break. Thresholds are in System, group Alerts.">
	<Panel title="Alerts" count={activeN ? `${activeN} active` : 'none active'} scroll={false} pad={false}>
		{#snippet toolbar()}
			<ListToolbar {l} rows={rows0} shown={res.rows.length} total={rows0.length} placeholder="Search alerts" />
			{#if ackErr}<p class="text-bad mt-2 text-[13px]" role="alert">{ackErr}</p>{/if}
		{/snippet}
		{#if loading}
			<div class="bg-raised m-3 h-40 animate-pulse rounded-lg" aria-busy="true"></div>
		{:else if error && rows0.length === 0}
			<div class="p-4">
				<EmptyState icon={CircleAlert} title="Could not load alerts" body={error}>
					{#snippet action()}<button type="button" class={btnGhost} onclick={load}>Try again</button>{/snippet}
				</EmptyState>
			</div>
		{:else}
			<DataTable label="Alerts" rows={res.rows} {cols} rowKey={(a) => a.id} sort={l.state.sort} dir={l.state.dir} onsort={(c) => l.sortBy(c)} rowH={vp.w < 700 ? 56 : 40}>
				{#snippet cell(a, c)}
					{#if c.key === 'severity'}<AlertChip severity={a.severity} />
					{:else if c.key === 'kind'}
						<span class="block truncate font-medium"><Hl text={KIND_LABEL[a.kind]} q={l.state.q} />{#if vp.w < 700 && a.node_id}<span class="text-muted-foreground font-normal"> · {host(a)}</span>{/if}</span>
						{#if vp.w < 1000}<span class="text-muted-foreground block truncate text-xs" title={a.detail}>{a.detail}</span>{/if}
					{:else if c.key === 'node'}{#if a.node_id}<a href="/nodes?q={encodeURIComponent(host(a))}" class="text-primary hover:underline"><Hl text={host(a)} q={l.state.q} /></a>{:else}<span class="text-muted-foreground">head</span>{/if}
					{:else if c.key === 'detail'}<span class="block truncate" title={a.detail}><Hl text={a.detail} q={l.state.q} /></span>
					{:else if c.key === 'since'}<span class="text-muted-foreground block whitespace-nowrap" title={datetime(a.since_ms)}>{ago(a.since_ms, tick)}</span>
						{#if vp.w < 700}
							{#if a.active && !a.acked_by && canAck}<button type="button" class="text-primary text-xs font-medium hover:underline max-sm:min-h-6" onclick={() => ack(a)}>Active · Ack</button>
							{:else}<span class="text-xs {a.active ? 'text-warn font-medium' : 'text-muted-foreground'}">{a.active ? (a.acked_by ? 'Active · acked' : 'Active') : 'Closed'}</span>{/if}
						{/if}
					{:else if c.key === 'status'}
						{#if a.active}
							<span class="text-warn text-xs font-medium">Active</span><span class="text-muted-foreground ml-1.5 text-xs">for {duration(tick - a.since_ms)}</span>
						{:else}
							<span class="text-muted-foreground text-xs">Closed after {duration(a.last_ms - a.since_ms)}</span>
						{/if}
						{#if a.acked_by}<span class="text-muted-foreground block truncate text-xs">acknowledged by {a.acked_by}</span>{/if}
					{:else if c.key === 'ack'}
						{#if a.active && !a.acked_by}<button type="button" class="{btnGhost} h-7 px-2 text-xs" onclick={() => ack(a)}><Check class="size-3.5" aria-hidden="true" />Ack</button>{/if}
					{/if}
				{/snippet}
				{#snippet empty()}
					<div class="p-4">
						{#if rows0.length === 0}
							<EmptyState icon={BellOff} title="No alerts so far" body="Nothing has been slow, flaky or about to break. An alert shows up here the moment a rule fires." />
						{:else}
							<EmptyState icon={BellOff} title="No alert matches these filters" body="Nothing fits the search and chips you chose.">
								{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
							</EmptyState>
						{/if}
					</div>
				{/snippet}
			</DataTable>
		{/if}
	</Panel>
</Page>
