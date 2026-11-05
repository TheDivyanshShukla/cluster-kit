<script lang="ts">
	import { CircleAlert, Lock, UserPlus } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { Role } from '#lib/api/schema.ts';
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

	type Row = { name: string; role: Role; created_ms: number };
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
		{ key: 'created', label: 'Created', w: '190px', sort: true }
	];
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
	subtitle="Who can sign in to this console, and what they may do."
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
				<DataTable label="Users" rows={res.rows} {cols} rowKey={(u) => u.name} sort={l.state.sort} dir={l.state.dir} onsort={(c) => l.sortBy(c)}>
					{#snippet cell(u, c)}
						{#if c.key === 'name'}<span class="block truncate font-medium"><Hl text={u.name} q={l.state.q} /></span>
						{:else if c.key === 'role'}{u.role}
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
