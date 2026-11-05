<script lang="ts">
	import CandidatesTable from '#lib/components/CandidatesTable.svelte';
	import HaPanel from '#lib/components/HaPanel.svelte';
	import HeadTopology from '#lib/components/HeadTopology.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import PowerPanel from '#lib/components/PowerPanel.svelte';
	import TunablesPanel from '#lib/components/TunablesPanel.svelte';
</script>

<Page
	title="System"
	subtitle="Everything adjustable in one place: the head and its standbys, power saving, and every timing and limit."
	tabs={[{ id: 'head', label: 'Head' }, { id: 'candidates', label: 'Candidates' }, { id: 'power', label: 'Power' }, { id: 'settings', label: 'Settings' }]}
	class="sm:grid-cols-2"
>
	<div class="contents min-h-0 sm:grid sm:grid-rows-[minmax(0,1.25fr)_minmax(0,1fr)] sm:gap-3">
		<Panel title="Head and standbys" tab="head">
			<HeadTopology size="full" />
			<div class="border-line mt-4 border-t pt-4"><HaPanel full bare /></div>
		</Panel>
		<Panel title="Power" tab="power"><PowerPanel bare /></Panel>
	</div>
	<div class="contents min-h-0 sm:grid sm:grid-rows-[minmax(0,1.1fr)_minmax(0,1.4fr)] sm:gap-3">
		<CandidatesTable />
		<TunablesPanel />
	</div>
</Page>
