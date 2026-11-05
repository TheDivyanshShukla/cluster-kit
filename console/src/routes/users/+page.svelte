<script lang="ts">
	import { CircleAlert, Lock, UserPlus } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { Role, type Quota } from '#lib/api/schema.ts';
	import DataTable, { type Col } from '#lib/components/DataTable.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import Hl from '#lib/components/Hl.svelte';
	import ListToolbar from '#lib/components/ListToolbar.svelte';
	import Page from '#lib/components/Page.svelte';
	import Panel from '#lib/components/Panel.svelte';
	import { Listing } from '#lib/listing.svelte.ts';
	import type { Spec } from '#lib/listing.ts';
	import { datetime } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnPrimary, field } from '#lib/ui.ts';

	type Row = { name: string; role: Role; created_ms: number; quota: Quota };
	const roles = Role.options;
	const about: Record<Role, string> = {
		viewer: 'Look only',
		researcher: 'Submit and cancel own jobs',
		operator: 'Cancel any job, run commands',
		admin: 'Everything, including users'
	};

	let users = $state<Row[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let name = $state('');
	let password = $state('');
	let role = $state<Role>('researcher');
	let busy = $state(false);
	let formError = $state<string | null>(null);
	let made = $state<string | null>(null);
	const spec: Spec<Row> = {
		facets: [{ key: 'role', label: 'Role', get: (u) => u.role, order: [...roles].reverse() }],
		text: (u) => [u.name, u.role],
		sorters: { name: (u) => u.name, role: (u) => roles.indexOf(u.role), created: (u) => u.created_ms },
		base: (a, b) => roles.indexOf(b.role) - roles.indexOf(a.role) || a.name.localeCompare(b.name, undefined, { numeric: true })
	};
	const l = new Listing<Row>(spec);
	const res = $derived(l.run(users));
	const cols: Col[] = [
		{ key: 'name', label: 'Name', w: 'minmax(120px,1fr)', sort: true },
		{ key: 'role', label: 'Role', w: '110px', sort: true },
		{ key: 'created', label: 'Created', w: '170px', sort: true },
		{ key: 'pcs', label: 'Max PCs', w: '96px' },
		{ key: 'run', label: 'Max running', w: '104px' },
		{ key: 'save', label: '', w: '84px' }
	];
	// quota editor: 0 means no limit. A draft exists only while the numbers differ from the saved ones.
	let draft = $state<Record<string, { pcs: number; run: number }>>({});
	let qBusy = $state('');
	let qError = $state<string | null>(null);
	const dv = (u: Row) => draft[u.name] ?? { pcs: u.quota.max_pcs, run: u.quota.max_running };
	const dirty = (u: Row) => dv(u).pcs !== u.quota.max_pcs || dv(u).run !== u.quota.max_running;
	const okNum = (n: unknown) => Number.isInteger(n) && (n as number) >= 0;
	function edit(u: Row, k: 'pcs' | 'run', v: number | null) {
		draft[u.name] = { ...dv(u), [k]: v as number };
	}
	async function saveQuota(u: Row) {
		const d = dv(u);
		qBusy = u.name;
		qError = null;
		try {
			await api.setQuota(u.name, { max_pcs: d.pcs, max_running: d.run });
			delete draft[u.name];
			await load();
		} catch (e) {
			qError = e instanceof ApiError ? e.message : 'Could not save the quota.';
		}
		qBusy = '';
	}
	const short = $derived(password.length > 0 && password.length < 12);

	async function load() {
		try {
			users = (await api.users()).users;
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Could not load users.';
		}
		loading = false;
	}
	$effect(() => {
		if (atLeast(roleNow(), 'admin')) void load();
	});

	async function create(e: SubmitEvent) {
		e.preventDefault();
		if (short) return;
		busy = true;
		formError = made = null;
		try {
			const r = await api.createUser({ name: name.trim(), password, role });
			made = `${r.user.name} can now sign in as ${r.user.role}.`;
			name = password = '';
			await load();
		} catch (err) {
			formError = err instanceof ApiError ? err.message : 'Something went wrong. Try again.';
		}
		busy = false;
	}
</script>

<Page
	title="Users"
	subtitle="Who can sign in, what they may do, and how many PCs and running jobs each may hold (0 = no limit)."
	tabs={[{ id: 'users', label: 'Users', badge: res.rows.length }, { id: 'add', label: 'Add a user' }]}
	class="sm:grid-cols-[minmax(0,1fr)_17rem] lg:grid-cols-[minmax(0,1fr)_20rem]"
>
	{#if !atLeast(roleNow(), 'admin')}
		<Panel title="Users" class="sm:col-span-2"><EmptyState icon={Lock} title="Admins only" body="Ask an admin if you need an account changed." /></Panel>
	{:else}
		<Panel title="Users" label="All users" count={users.length} scroll={false} pad={false}>
			{#snippet toolbar()}
				<ListToolbar {l} rows={users} shown={res.rows.length} total={users.length} placeholder="Search name or role" />
			{/snippet}
			{#if loading}
				<div class="bg-raised m-3 h-32 animate-pulse rounded-lg" aria-busy="true"></div>
			{:else if error}
				<div class="p-4"><EmptyState icon={CircleAlert} title="Could not load users" body={error} /></div>
			{:else}
				{#if qError}<p class="text-bad px-3 pt-2 text-[13px]" role="alert">{qError}</p>{/if}
				<DataTable label="Users" rows={res.rows} {cols} minW={720} rowKey={(u) => u.name} sort={l.state.sort} dir={l.state.dir} onsort={(c) => l.sortBy(c)}>
					{#snippet cell(u, c)}
						{#if c.key === 'name'}<span class="block truncate font-medium"><Hl text={u.name} q={l.state.q} /></span>
						{:else if c.key === 'role'}{u.role}
						{:else if c.key === 'pcs' || c.key === 'run'}
							<input
								class="{field} num h-8 w-full px-2 py-1 text-right"
								type="number"
								min="0"
								step="1"
								inputmode="numeric"
								aria-label="{c.key === 'pcs' ? 'Most PCs' : 'Most running jobs'} for {u.name} (0 = no limit)"
								value={c.key === 'pcs' ? dv(u).pcs : dv(u).run}
								oninput={(e) => edit(u, c.key as 'pcs' | 'run', e.currentTarget.valueAsNumber)}
							/>
						{:else if c.key === 'save'}
							{#if dirty(u)}<button type="button" class="{btnPrimary} h-8 px-3" disabled={qBusy === u.name || !okNum(dv(u).pcs) || !okNum(dv(u).run)} onclick={() => saveQuota(u)}>Save</button>{/if}
						{:else}<span class="text-muted-foreground whitespace-nowrap">{datetime(u.created_ms)}</span>{/if}
					{/snippet}
					{#snippet empty()}
						<div class="p-4">
							<EmptyState icon={Lock} title="No user matches these filters" body="Nothing fits the search and chips you chose.">
								{#snippet action()}<button type="button" class="text-primary text-[13px] font-medium hover:underline" onclick={() => l.clear()}>Clear filters</button>{/snippet}
							</EmptyState>
						</div>
					{/snippet}
				</DataTable>
			{/if}
		</Panel>

		<Panel title="Add a user" tab="add">
			<form onsubmit={create} class="space-y-4" aria-labelledby="new-user">
				<h3 id="new-user" class="sr-only">Add a user</h3>
				<label class="block">
					<span class="mb-1 block text-xs font-medium">Name</span>
					<input class={field} bind:value={name} required autocomplete="off" autocapitalize="none" spellcheck="false" />
				</label>
				<label class="block">
					<span class="mb-1 block text-xs font-medium">Password</span>
					<input class={field} type="password" bind:value={password} required autocomplete="new-password" aria-describedby="np-hint" />
					<span id="np-hint" class="mt-1 block text-xs {short ? 'text-bad' : 'text-muted-foreground'}">At least 12 characters.</span>
				</label>
				<label class="block">
					<span class="mb-1 block text-xs font-medium">Role</span>
					<select class="{field} appearance-none" bind:value={role}>
						{#each roles as r (r)}<option value={r}>{r}</option>{/each}
					</select>
					<span class="text-muted-foreground mt-1 block text-xs">{about[role]}</span>
				</label>
				{#if formError}<p class="text-bad flex items-start gap-1.5 text-[13px]" role="alert"><CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />{formError}</p>{/if}
				{#if made}<p class="text-ok text-[13px]" role="status">{made}</p>{/if}
				<button type="submit" class="{btnPrimary} w-full" disabled={busy || !name.trim() || !password || short}>
					<UserPlus class="size-4" aria-hidden="true" />{busy ? 'Creating…' : 'Create user'}
				</button>
			</form>
		</Panel>
	{/if}
</Page>
