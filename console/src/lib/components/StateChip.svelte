<script lang="ts">
	import { Ban, CircleCheck, CircleX, Clock, LoaderCircle } from '@lucide/svelte';
	import type { JobState } from '#lib/api/schema.ts';

	let { state }: { state: JobState } = $props();
	// Colour is never the only signal: every state has its own icon and word.
	const meta = {
		queued: { icon: Clock, label: 'Queued', cls: 'text-muted-foreground bg-raised' },
		running: { icon: LoaderCircle, label: 'Running', cls: 'text-busy bg-busy/10' },
		done: { icon: CircleCheck, label: 'Done', cls: 'text-ok bg-ok/10' },
		cancelled: { icon: Ban, label: 'Cancelled', cls: 'text-muted-foreground bg-raised' },
		failed: { icon: CircleX, label: 'Failed', cls: 'text-bad bg-bad/10' }
	} as const;
	const m = $derived(meta[state]);
</script>

<span class="inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium {m.cls}">
	<m.icon class="size-3.5 shrink-0 {state === 'running' ? 'motion-safe:animate-spin' : ''}" strokeWidth={2} aria-hidden="true" />{m.label}
</span>
