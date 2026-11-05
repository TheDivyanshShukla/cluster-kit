<script lang="ts">
	import '../app.css';
	import { page } from '$app/state';
	import { CalendarClock, CircleAlert, Gauge, ListChecks, Radio, LogOut, Map as MapIcon, Moon, MonitorPlay, Server, Settings, SlidersHorizontal, Sun, User as UserIcon, Users, WifiOff } from '@lucide/svelte';
	import type { LayoutProps } from './$types';
	import AuthForm from '#lib/components/AuthForm.svelte';
	import EmptyState from '#lib/components/EmptyState.svelte';
	import HaBanners from '#lib/components/HaBanners.svelte';
	import { history } from '#lib/canvas/history.ts';
	import { isStandby, live, startLive } from '#lib/live.svelte.ts';
	import { atLeast } from '#lib/jobs.ts';
	import { authed, loadMe, roleNow, session, signOut, userNow } from '#lib/session.svelte.ts';
	import { btnGhost } from '#lib/ui.ts';

	let { children }: LayoutProps = $props();
	$effect(() => void loadMe());
	// The stream only runs while someone is signed in (or the head is in open mode).
	$effect(() => (authed() ? startLive() : undefined));
	// the Live page's 30 minute sparkline is filled from every cluster snapshot, on any page
	$effect(() => {
		const c = live.cluster;
		if (c) history.push(c.workers, Date.now());
	});

	const nav = $derived(
		[
			{ href: '/', label: 'Overview', icon: Gauge, show: true },
			{ href: '/nodes', label: 'Nodes', icon: Server, show: true },
			{ href: '/map', label: 'Floor map', icon: MapIcon, show: true },
			{ href: '/live', label: 'Live', icon: Radio, show: true },
			{ href: '/jobs', label: 'Jobs', icon: ListChecks, show: true },
			{ href: '/policies', label: 'Policies', icon: SlidersHorizontal, show: true },
			{ href: '/events', label: 'Events', icon: CalendarClock, show: true },
			{ href: '/wall', label: 'Wall mode', icon: MonitorPlay, show: true },
			{ href: '/settings', label: 'System', icon: Settings, show: atLeast(roleNow(), 'operator') },
			{ href: '/users', label: 'Users', icon: Users, show: atLeast(roleNow(), 'admin') }
		].filter((n) => n.show)
	);
	const isActive = (href: string) => (href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href));

	let dark = $state(
		typeof document !== 'undefined' &&
			(document.documentElement.dataset.theme === 'dark' ||
				(!document.documentElement.dataset.theme && matchMedia('(prefers-color-scheme: dark)').matches))
	);
	function toggleTheme() {
		dark = !dark;
		document.documentElement.dataset.theme = dark ? 'dark' : 'light';
		try {
			localStorage.setItem('ck-theme', dark ? 'dark' : 'light');
		} catch {}
	}

	let menu = $state<HTMLDetailsElement>();
	const online = $derived(live.cluster?.workers.length ?? 0);
</script>

<svelte:head><title>Cluster Kit</title></svelte:head>
<svelte:window
	onclick={(e) => menu?.open && !menu.contains(e.target as Node) && (menu.open = false)}
	onkeydown={(e) => e.key === 'Escape' && menu?.open && (menu.open = false)}
/>

{#if session.loading}
	<div class="flex min-h-dvh items-center justify-center" aria-busy="true"><div class="bg-raised h-2 w-32 animate-pulse rounded-full"></div></div>
{:else if session.error && !session.me}
	<div class="mx-auto max-w-md px-4 py-16">
		<EmptyState icon={CircleAlert} title="The head is not answering" body={session.error}>
			{#snippet action()}<button type="button" class={btnGhost} onclick={() => void loadMe()}>Try again</button>{/snippet}
		</EmptyState>
	</div>
{:else if !authed()}
	<AuthForm bootstrap={session.me?.needs_bootstrap ?? false} />
{:else if page.url.pathname === '/wall'}
	{@render children()}
{:else}
	<div class="flex min-h-dvh flex-col md:flex-row">
		<aside class="border-line bg-surface flex shrink-0 items-center gap-1 border-b px-3 py-2 md:sticky md:top-0 md:h-dvh md:w-52 md:flex-col md:items-stretch md:border-r md:border-b-0 md:px-3 md:py-4">
			<a href="/" class="mr-2 flex items-center gap-2 px-2 font-semibold tracking-tight md:mr-0 md:mb-5" aria-label="Cluster Kit home">
				<span class="bg-primary size-2.5 shrink-0 rounded-sm" aria-hidden="true"></span><span class="hidden md:inline">Cluster Kit</span>
			</a>
			<nav class="flex min-w-0 gap-0.5 overflow-x-auto md:flex-col md:gap-1 md:overflow-visible" aria-label="Main">
				{#each nav as item (item.href)}
					{@const active = isActive(item.href)}
					<a
						href={item.href}
						title={item.label}
						aria-current={active ? 'page' : undefined}
						class="flex shrink-0 items-center gap-2.5 rounded-lg px-2 py-2 text-[13px] md:px-2.5 transition-colors md:py-1.5 {active
							? 'bg-raised text-foreground font-medium'
							: 'text-muted-foreground hover:bg-raised hover:text-foreground'}"
					>
						<item.icon class="size-4 shrink-0" strokeWidth={1.75} aria-hidden="true" /><span class="max-md:sr-only">{item.label}</span>
					</a>
				{/each}
			</nav>
			<div class="ml-auto flex items-center gap-1 md:mt-auto md:ml-0 md:flex-col md:items-stretch">
				<p class="text-muted-foreground hidden px-2.5 pb-1 text-xs md:block" aria-live="polite">
					{#if live.link === 'reconnecting'}
						<span class="text-warn inline-flex items-center gap-1"><WifiOff class="size-3.5" aria-hidden="true" />Reconnecting…</span>
					{:else}
						<span class="num">{online}</span> PC{online === 1 ? '' : 's'} connected
					{/if}
				</p>
				<button
					type="button"
					onclick={toggleTheme}
					class="text-muted-foreground hover:bg-raised hover:text-foreground flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-[13px] md:py-1.5"
					aria-label={dark ? 'Switch to light theme' : 'Switch to dark theme'}
				>
					{#if dark}<Sun class="size-4" strokeWidth={1.75} aria-hidden="true" />{:else}<Moon class="size-4" strokeWidth={1.75} aria-hidden="true" />{/if}
					<span class="hidden md:inline">{dark ? 'Light' : 'Dark'} theme</span>
				</button>
				<details bind:this={menu} class="relative">
					<summary
						class="text-muted-foreground hover:bg-raised hover:text-foreground flex cursor-pointer list-none items-center gap-2.5 rounded-lg px-2.5 py-2 text-[13px] marker:hidden md:py-1.5"
						aria-label="Account menu"
					>
						<UserIcon class="size-4 shrink-0" strokeWidth={1.75} aria-hidden="true" />
						<span class="hidden min-w-0 truncate md:inline">{userNow() ?? 'Open mode'}</span>
					</summary>
					<div class="bg-surface border-line absolute top-full right-0 z-20 mt-1 w-52 rounded-lg border p-3 shadow-lg md:top-auto md:right-auto md:bottom-full md:left-0 md:mb-1">
						<p class="truncate font-medium">{userNow() ?? 'Anonymous'}</p>
						<p class="text-muted-foreground text-xs">Role: {roleNow()}{session.me?.mode === 'open' ? ' · open mode' : ''}</p>
						{#if session.me?.user}
							<button type="button" class="{btnGhost} mt-3 w-full" onclick={() => void signOut()}>
								<LogOut class="size-4" aria-hidden="true" />Sign out
							</button>
						{/if}
					</div>
				</details>
			</div>
		</aside>
		<main class="min-w-0 flex-1 px-4 py-5 md:px-8 md:py-7">
			{#if isStandby()}<HaBanners />{/if}
			{#if live.link === 'reconnecting' && !isStandby()}
				<p class="bg-raised border-line text-warn mb-4 flex items-center gap-2 rounded-lg border px-3 py-2 text-[13px]" role="status">
					<WifiOff class="size-4 shrink-0" aria-hidden="true" />Lost the live link to the head. Reconnecting; numbers may be a few seconds old.
				</p>
			{/if}
			<fieldset disabled={isStandby()} class="contents">{@render children()}</fieldset>
		</main>
	</div>
{/if}
