<script lang="ts">
	import EventsView from '#lib/components/EventsView.svelte';
	import LogsView from '#lib/components/LogsView.svelte';
	import Page from '#lib/components/Page.svelte';
	import { followRouter, params, setQuery } from '#lib/urlstate.svelte.ts';

	// One page, two sources with the same toolbar: the audit events (who did what) and the head's structured logs.
	followRouter();
	const view = $derived(params().get('view') === 'logs' ? 'logs' : 'events');
	const views = [{ id: 'events', label: 'Events' }, { id: 'logs', label: 'Logs' }];
</script>

<Page
	title={view === 'logs' ? 'Logs' : 'Events'}
	subtitle={view === 'logs' ? 'Structured log lines from the head, newest first.' : 'The audit log: who did what, and what the cluster did.'}
	tabs={[{ id: 'list', label: 'List' }, { id: 'detail', label: 'Detail' }]}
	class="sm:grid-cols-[minmax(0,1fr)_17rem] lg:grid-cols-[minmax(0,1fr)_24rem]"
>
	{#snippet actions()}
		<div class="bg-raised border-line inline-flex rounded-lg border p-0.5" role="group" aria-label="Source">
			{#each views as v (v.id)}
				<button
					type="button"
					aria-pressed={view === v.id}
					onclick={() => setQuery('view', v.id === 'events' ? null : v.id)}
					class="min-h-8 rounded-md px-3 text-[13px] {view === v.id ? 'bg-surface text-foreground font-medium shadow-sm' : 'text-muted-foreground hover:text-foreground'}"
				>{v.label}</button>
			{/each}
		</div>
	{/snippet}
	{#key view}
		{#if view === 'logs'}<LogsView />{:else}<EventsView />{/if}
	{/key}
</Page>
