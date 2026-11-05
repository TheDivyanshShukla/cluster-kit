<script lang="ts">
	import { CircleAlert, Lock, UserPlus } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import { Role } from '#lib/api/schema.ts';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import { datetime } from '#lib/format.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { roleNow } from '#lib/session.svelte.ts';
	import { btnPrimary, card, field } from '#lib/ui.ts';

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

<svelte:head><title>Users · Cluster Kit</title></svelte:head>

<h1 class="text-xl font-semibold tracking-tight">Users</h1>
<p class="text-muted-foreground mt-1">Who can sign in to this console, and what they may do.</p>

{#if !atLeast(roleNow(), 'admin')}
	<div class="mt-6"><EmptyState icon={Lock} title="Admins only" body="Ask an admin if you need an account changed." /></div>
{:else}
	<div class="mt-5 grid items-start gap-5 lg:grid-cols-[1fr_20rem]">
		<section aria-label="All users">
			{#if loading}
				<div class="{card} h-40 animate-pulse" aria-busy="true"></div>
			{:else if error}
				<EmptyState icon={CircleAlert} title="Could not load users" body={error} />
			{:else}
				<div class="{card} relative overflow-x-auto">
					<table class="w-full text-left text-[13px]">
						<thead class="text-muted-foreground border-line border-b text-xs">
							<tr><th class="px-3 py-2 font-medium">Name</th><th class="px-3 py-2 font-medium">Role</th><th class="px-3 py-2 font-medium">Created</th></tr>
						</thead>
						<tbody>
							{#each users as u (u.name)}
								<tr class="border-line border-b last:border-0">
									<td class="px-3 py-2 font-medium">{u.name}</td>
									<td class="px-3 py-2">{u.role}</td>
									<td class="text-muted-foreground px-3 py-2 whitespace-nowrap">{datetime(u.created_ms)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</section>

		<form onsubmit={create} class="{card} space-y-4 p-4" aria-labelledby="new-user">
			<h2 id="new-user" class="font-medium">Add a user</h2>
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
	</div>
{/if}
