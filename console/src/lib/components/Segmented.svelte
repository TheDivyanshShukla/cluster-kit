<script lang="ts" generics="T extends string">
	interface Props {
		legend: string;
		options: { value: T; label: string }[];
		value: T;
	}
	let { legend, options, value = $bindable() }: Props = $props();
	const name = `seg-${Math.random().toString(36).slice(2, 8)}`;
</script>

<fieldset class="m-0 border-0 p-0">
	<legend class="sr-only">{legend}</legend>
	<div class="bg-raised border-line inline-flex max-w-full flex-wrap rounded-lg border p-0.5">
		{#each options as o (o.value)}
			<label class="has-[:checked]:bg-surface has-[:checked]:text-foreground text-muted-foreground has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-ring cursor-pointer rounded-md px-3 py-1 text-[13px] has-[:checked]:font-medium has-[:checked]:shadow-sm">
				<input type="radio" {name} value={o.value} bind:group={value} class="sr-only" />{o.label}
			</label>
		{/each}
	</div>
</fieldset>
