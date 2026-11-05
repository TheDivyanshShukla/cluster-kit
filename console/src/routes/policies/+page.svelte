<script lang="ts">
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import BookingsPanel from '#lib/components/BookingsPanel.svelte';
	import TimetablePanel from '#lib/components/TimetablePanel.svelte';
	import CapsPanel from '#lib/components/CapsPanel.svelte';
	import ConfigPanel from '#lib/components/ConfigPanel.svelte';
	import PoolsPanel from '#lib/components/PoolsPanel.svelte';
	import ReservesPanel from '#lib/components/ReservesPanel.svelte';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';

	const canEdit = $derived(atLeast(roleNow(), 'operator'));
</script>

<Page
	title="Policies"
	subtitle="Timetable, bookings, pools, thread caps and reserves decide which PCs jobs may use."
	always
	tabs={[
		{ id: 'timetable', label: 'Timetable' },
		{ id: 'bookings', label: 'Bookings' },
		{ id: 'reserves', label: 'Reserves' },
		{ id: 'caps', label: 'Caps' },
		{ id: 'pools', label: 'Pools' },
		...(atLeast(roleNow(), 'admin') ? [{ id: 'config', label: 'Config' }] : [])
	]}
>
	<Panel title="Timetable" tab="timetable"><TimetablePanel {canEdit} /></Panel>
	<Panel title="Bookings" tab="bookings"><BookingsPanel {canEdit} /></Panel>
	<Panel title="Reserves" tab="reserves"><ReservesPanel {canEdit} /></Panel>
	<Panel title="Thread caps" tab="caps"><CapsPanel {canEdit} /></Panel>
	<Panel title="Pools" tab="pools"><PoolsPanel {canEdit} /></Panel>
	{#if atLeast(roleNow(), 'admin')}<Panel title="Setup backup" tab="config"><ConfigPanel /></Panel>{/if}
</Page>
