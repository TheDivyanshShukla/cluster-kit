<script lang="ts">
	import { CircleAlert, Gauge, Lock, Radar } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { Net, NetNode } from '#lib/api/schema.ts';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { ago, bytes, mbps } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnGhost, btnPrimary } from '#lib/ui.ts';

	let net = $state<Net | null>(null);
	let error = $state<string | null>(null);
	let busy = $state<'' | 'each' | 'burst'>('');
	let probeError = $state('');
	let probed = $state('');
	const canProbe = $derived(atLeast(roleNow(), 'operator'));

	async function load() {
		try {
			net = await api.net();
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not read the network figures.';
		}
	}
	$effect(() => {
		void load();
		const t = setInterval(load, 10_000);
		return () => clearInterval(t);
	});

	async function probe(burst: boolean) {
		busy = burst ? 'burst' : 'each';
		probeError = probed = '';
		try {
			await api.netProbe([], burst);
			probed = burst ? 'Every PC is sending at once. The head uplink updates in a few seconds.' : 'Each PC is measuring its own link, two at a time.';
			setTimeout(load, 3000);
		} catch (e) {
			probeError = e instanceof ApiError ? e.message : 'Could not start the probe.';
		}
		busy = '';
	}

	const slowest = (a: NetNode, b: NetNode) => (a.net_mbps ?? Infinity) - (b.net_mbps ?? Infinity) || a.host.localeCompare(b.host, undefined, { numeric: true });
	const pcs = $derived([...(net?.nodes ?? [])].sort(slowest));
	const labs = $derived([...(net?.labs ?? [])].sort((a, b) => a.lab.localeCompare(b.lab, undefined, { numeric: true })));
	const unmeasured = $derived(pcs.filter((n) => n.net_mbps == null).length);
	const pcCols: Col[] = [
		{ key: 'host', label: 'PC', w: 'minmax(100px,1.4fr)' },
		{ key: 'lab', label: 'Lab', w: 'minmax(80px,1fr)' },
		{ key: 'speed', label: 'Link speed', w: '110px', right: true },
		{ key: 'rtt', label: 'Round trip', w: '90px', right: true },
		{ key: 'when', label: 'Measured', w: '100px', right: true }
	];
	const labCols: Col[] = [
		{ key: 'lab', label: 'Lab', w: 'minmax(80px,1fr)' },
		{ key: 'pcs', label: 'PCs measured', w: '120px', right: true },
		{ key: 'speed', label: 'Typical link', w: '110px', right: true }
	];
</script>

<Page
	title="Network"
	subtitle="How fast each PC can pull files, and how much the head can send at once. Bulk jobs plan with these numbers."
	tabs={[{ id: 'summary', label: 'Summary' }, { id: 'labs', label: 'Labs', badge: labs.length }, { id: 'pcs', label: 'PCs', badge: pcs.length }]}
	class="sm:grid-cols-[minmax(0,1fr)_minmax(0,1.6fr)] sm:grid-rows-[auto_minmax(0,1fr)]"
>
	{#snippet actions()}
		<button type="button" class={btnGhost} disabled={!canProbe || !!busy} title={canProbe ? 'Each PC measures its own link' : `Probing needs the operator role. You are ${roleNow()}.`} onclick={() => probe(false)}>
			<Radar class="size-4" aria-hidden="true" />{busy === 'each' ? 'Starting…' : 'Probe each PC'}
		</button>
		<button type="button" class={btnPrimary} disabled={!canProbe || !!busy} title={canProbe ? 'Every PC sends at once, to learn the head uplink' : `Probing needs the operator role. You are ${roleNow()}.`} onclick={() => probe(true)}>
			<Gauge class="size-4" aria-hidden="true" />{busy === 'burst' ? 'Starting…' : 'Measure head uplink'}
		</button>
	{/snippet}

	{#if error && !net}
		<Panel title="Network" tab="summary" class="sm:col-span-2 sm:row-span-2"><EmptyState icon={CircleAlert} title="Could not read the network" body={error} /></Panel>
	{:else if !net}
		<Panel title="Network" tab="summary" class="sm:col-span-2 sm:row-span-2"><div class="bg-raised h-32 animate-pulse rounded-lg" aria-busy="true"></div></Panel>
	{:else}
		<Panel title="Head uplink" tab="summary" scroll={false} class="sm:col-span-2">
			<div class="flex flex-wrap items-end gap-x-10 gap-y-3">
				<div>
					<p class="text-muted-foreground text-xs">Head uplink</p>
					<p class="num text-2xl font-semibold">{mbps(net.head_uplink_mbps)}</p>
					{#if net.head_uplink_assumed}
						<p class="text-warn mt-0.5 text-xs">A guess. Run "Measure head uplink" for a real figure.</p>
					{:else}<p class="text-muted-foreground mt-0.5 text-xs">Measured</p>{/if}
				</div>
				<div>
					<p class="text-muted-foreground text-xs">Sent by the head</p>
					<p class="num text-2xl font-semibold">{bytes(net.head_bytes_sent)}</p>
					<p class="text-muted-foreground mt-0.5 text-xs">Files and results since it started</p>
				</div>
				<div>
					<p class="text-muted-foreground text-xs">PCs measured</p>
					<p class="num text-2xl font-semibold">{pcs.length - unmeasured}<span class="text-muted-foreground ml-1 text-base">of {pcs.length}</span></p>
					<p class="text-muted-foreground mt-0.5 text-xs">{unmeasured ? `${unmeasured} not measured yet` : 'All measured'}</p>
				</div>
			</div>
			{#if !canProbe}<p class="text-muted-foreground mt-3 flex items-center gap-1.5 text-[13px]"><Lock class="size-3.5" aria-hidden="true" />Probing needs the operator role. You are signed in as {roleNow()}.</p>{/if}
			{#if probed}<p class="text-ok mt-3 text-[13px]" role="status">{probed}</p>{/if}
			{#if probeError}<p class="text-bad mt-3 text-[13px]" role="alert">{probeError}</p>{/if}
		</Panel>

		<Panel title="Labs" tab="labs" scroll={false} pad={false} count={labs.length}>
			<DataTable label="Link speed by lab" rows={labs} cols={labCols} rowKey={(r) => r.lab}>
				{#snippet cell(r, c)}
					{#if c.key === 'lab'}<span class="block truncate font-medium">{r.lab}</span>
					{:else if c.key === 'pcs'}<span class="num">{r.pcs}</span>
					{:else}<span class="num">{mbps(r.mbps)}</span>{/if}
				{/snippet}
				{#snippet empty()}<div class="p-4"><EmptyState icon={Radar} title="No lab has been measured" body="Probe each PC to fill this in." /></div>{/snippet}
			</DataTable>
		</Panel>

		<Panel title="PCs, slowest first" tab="pcs" scroll={false} pad={false} count={pcs.length}>
			<DataTable label="Link speed by PC" rows={pcs} cols={pcCols} rowKey={(r) => r.node_id} minW={440}>
				{#snippet cell(r, c)}
					{#if c.key === 'host'}<span class="block truncate font-medium">{r.host}</span>
					{:else if c.key === 'lab'}<span class="block truncate">{r.lab || 'Unplaced'}</span>
					{:else if c.key === 'speed'}<span class="num {r.net_mbps != null && r.net_mbps < 200 ? 'text-warn' : ''}">{mbps(r.net_mbps)}</span>
					{:else if c.key === 'rtt'}<span class="num">{r.rtt_ms == null ? '—' : `${r.rtt_ms.toFixed(1)} ms`}</span>
					{:else}<span class="num text-muted-foreground">{r.probed_ms == null ? 'never' : ago(r.probed_ms)}</span>{/if}
				{/snippet}
				{#snippet empty()}<div class="p-4"><EmptyState icon={Radar} title="No PC is connected" body="Real PCs appear here once they join." /></div>{/snippet}
			</DataTable>
		</Panel>
	{/if}
</Page>
