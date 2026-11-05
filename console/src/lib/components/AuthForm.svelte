<script lang="ts">
	import { CircleAlert, LogIn, ShieldPlus } from '@lucide/svelte';
	import { ApiError } from '#lib/api/client.ts';
	import { signIn } from '#lib/session.svelte.ts';
	import { btnPrimary, field } from '#lib/ui.ts';

	let { bootstrap }: { bootstrap: boolean } = $props();
	let name = $state('');
	let password = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);
	const short = $derived(bootstrap && password.length > 0 && password.length < 12);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		if (short) return;
		busy = true;
		error = null;
		try {
			await signIn(name.trim(), password, bootstrap);
		} catch (err) {
			error = err instanceof ApiError ? err.message : 'Something went wrong. Try again.';
		}
		busy = false;
	}
</script>

<div class="flex min-h-dvh items-center justify-center px-4 py-10">
	<form onsubmit={submit} class="bg-surface border-line w-full max-w-sm rounded-lg border p-6" aria-labelledby="auth-title">
		<p class="flex items-center gap-2 font-semibold tracking-tight">
			<span class="bg-primary size-2.5 rounded-sm" aria-hidden="true"></span>Cluster Kit
		</p>
		<h1 id="auth-title" class="mt-5 text-xl font-semibold tracking-tight">{bootstrap ? 'Create the first admin' : 'Sign in'}</h1>
		<p class="text-muted-foreground mt-1">
			{bootstrap
				? 'No user exists yet. This works only on the PC that runs the head (127.0.0.1).'
				: 'Use the account an admin made for you.'}
		</p>

		<label class="mt-5 block">
			<span class="mb-1 block text-xs font-medium">Name</span>
			<input class={field} bind:value={name} autocomplete="username" required autocapitalize="none" spellcheck="false" />
		</label>
		<label class="mt-3 block">
			<span class="mb-1 block text-xs font-medium">Password</span>
			<input class={field} type="password" bind:value={password} autocomplete={bootstrap ? 'new-password' : 'current-password'} required aria-describedby="pw-hint" />
			{#if bootstrap}
				<span id="pw-hint" class="mt-1 block text-xs {short ? 'text-bad' : 'text-muted-foreground'}">At least 12 characters.</span>
			{/if}
		</label>

		{#if error}
			<p class="text-bad mt-3 flex items-start gap-1.5 text-[13px]" role="alert">
				<CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />{error}
			</p>
		{/if}
		<button type="submit" class="{btnPrimary} mt-5 w-full" disabled={busy || !name.trim() || !password || short}>
			{#if bootstrap}<ShieldPlus class="size-4" aria-hidden="true" />{:else}<LogIn class="size-4" aria-hidden="true" />{/if}
			{busy ? 'Working…' : bootstrap ? 'Create admin' : 'Sign in'}
		</button>
	</form>
</div>
