<script lang="ts">
	import type { Snippet } from 'svelte';
	import { CircleAlert, CircleHelp, Crosshair } from '@lucide/svelte';
	import { api, ApiError } from '#lib/api/client.ts';
	import type { SelectPreview } from '#lib/api/schema.ts';
	import { debounce, plural, previewState, previewText } from '#lib/org.ts';
	import { field } from '#lib/ui.ts';

	interface Props {
		/** the selector text */
		value: string;
		label?: string;
		/** what an empty selector means, shown as the count line */
		emptyMeans?: string;
		placeholder?: string;
		/** matching node ids (null while unknown or invalid) */
		matched?: string[] | null;
		id?: string;
		actions?: Snippet;
		/** with a count, say what the matches can do: "23 PCs can be a standby" */
		matchWord?: string;
	}
	let { value = $bindable(''), label = 'Selector', emptyMeans = 'Every PC', placeholder = 'lab=A state=active cores>=8', matched = $bindable(null), id = 'sel', actions, matchWord }: Props = $props();

	let preview = $state<SelectPreview | null>(null);
	let failure = $state<string | null>(null);
	let seq = 0;
	const run = debounce(async (q: string) => {
		const mine = ++seq;
		try {
			const p = await api.select(q);
			if (mine !== seq) return;
			preview = p;
			failure = null;
		} catch (e) {
			if (mine !== seq) return;
			preview = null;
			failure = e instanceof ApiError ? e.message : 'Could not check the selector.';
		}
	}, 250);

	$effect(() => {
		const q = value.trim();
		preview = null; // the old count is stale the moment the text changes
		failure = null;
		if (!q) {
			seq++;
			run.cancel();
			return;
		}
		run(q);
		return run.cancel;
	});
	const st = $derived(previewState(value, preview, failure));
	$effect(() => {
		matched = st.kind === 'count' ? (preview?.node_ids ?? null) : null;
	});

	const examples = [
		['lab=A', 'every PC in Lab A'],
		['lab=A,B state=active', 'active PCs in Lab A or B'],
		['cores>=8 ram_gb>=16', 'at least 8 cores and 16 GB'],
		['pool=night !broken', 'in pool night, not labelled broken'],
		['host=lab-a*', 'host name starts with lab-a'],
		['class="Intel i5-10400F/12c/16G"', 'one hardware class']
	];
	let help = $state<HTMLDetailsElement>();
</script>

<svelte:window
	onclick={(e) => help?.open && !help.contains(e.target as Node) && (help.open = false)}
	onkeydown={(e) => e.key === 'Escape' && help?.open && (help.open = false)}
/>

<div>
	<div class="flex items-center justify-between gap-2">
		<label for={id} class="text-xs font-medium">{label}</label>
		<details bind:this={help} class="relative">
			<summary class="text-muted-foreground hover:text-foreground inline-flex cursor-pointer list-none items-center gap-1 text-xs marker:hidden">
				<CircleHelp class="size-3.5" aria-hidden="true" />Syntax
			</summary>
			<div class="bg-surface border-line absolute top-full right-0 z-20 mt-1 w-[min(22rem,calc(100vw-2rem))] rounded-lg border p-3 text-xs shadow-lg">
				<p class="text-muted-foreground">Space-separated terms; every term must hold. Empty means {emptyMeans.toLowerCase()}.</p>
				<ul class="mt-2 space-y-1.5">
					{#each examples as [code, text] (code)}
						<li>
							<button type="button" class="hover:bg-raised block w-full rounded-md px-1.5 py-1 text-left" onclick={() => ((value = code), (help!.open = false))}>
								<code class="font-mono">{code}</code><span class="text-muted-foreground block">{text}</span>
							</button>
						</li>
					{/each}
				</ul>
				<p class="text-muted-foreground mt-2">Operators: = != &gt;= &lt;= &gt; &lt; · <code class="font-mono">key</code> present · <code class="font-mono">!key</code> absent · a,b any · pre* prefix. Keys: node host addr state online lab x y pool cores threads ram_gb cpu arch os class virtual, plus your labels.</p>
			</div>
		</details>
	</div>
	<div class="mt-1 flex gap-2">
		<div class="relative min-w-0 flex-1">
			<Crosshair class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2" aria-hidden="true" />
			<input
				{id}
				bind:value
				{placeholder}
				class="{field} pl-9 font-mono"
				autocomplete="off"
				spellcheck="false"
				maxlength="400"
				aria-invalid={st.kind === 'error'}
				aria-describedby="{id}-count"
			/>
		</div>
		{@render actions?.()}
	</div>
	<p id="{id}-count" class="mt-1 flex items-start gap-1.5 text-xs {st.kind === 'error' ? 'text-bad' : st.kind === 'count' && st.count === 0 ? 'text-warn' : 'text-muted-foreground'}" role="status" aria-live="polite">
		{#if st.kind === 'error'}<CircleAlert class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />{/if}
		<span class="num break-words">{st.kind === 'count' && st.count > 0 && matchWord ? `${plural(st.count, 'PC')} ${matchWord}` : previewText(st, emptyMeans)}</span>
	</p>
</div>
