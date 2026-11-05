<script lang="ts">
	import { CircleAlert, Info, TriangleAlert } from '@lucide/svelte';
	import type { Alert } from '#lib/api/schema.ts';
	import { SEV_LABEL } from '#lib/alerts.ts';

	// Severity as icon + word + colour, so colour is never the only signal.
	let { severity }: { severity: Alert['severity'] } = $props();
	const m = {
		bad: { icon: CircleAlert, cls: 'text-bad bg-bad/10' },
		warn: { icon: TriangleAlert, cls: 'text-warn bg-warn/10' },
		info: { icon: Info, cls: 'text-muted-foreground bg-raised' }
	} as const;
	const x = $derived(m[severity]);
</script>

<span class="inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium {x.cls}"><x.icon class="size-3.5 shrink-0" strokeWidth={2} aria-hidden="true" />{SEV_LABEL[severity]}</span>
