<script lang="ts">
	let { value, label = 'Progress', tone = 'bg-primary' }: { value: number; label?: string; tone?: string } = $props();
	const pct = $derived(Math.round(Math.max(0, Math.min(1, value)) * 100));
</script>

<div class="bg-raised h-2 overflow-hidden rounded-full" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-label={label}>
	<div class="{tone} h-full rounded-full transition-[width] duration-500" style="width: {pct}%"></div>
</div>
