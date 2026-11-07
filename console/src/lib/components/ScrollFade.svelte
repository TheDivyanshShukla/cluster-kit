<script lang="ts">
	import type { Snippet } from 'svelte';
	// A scrolling area that fills its parent, with a thin scrollbar and a hairline at the edge that has more content.
	interface Props {
		el?: HTMLElement;
		viewH?: number;
		top?: number;
		onscroll?: (e: Event) => void;
		/** px to push the top fade down (a sticky header sits there) */
		fadeTop?: number;
		class?: string;
		children: Snippet;
	}
	let { el = $bindable(), viewH = $bindable(0), top = $bindable(0), onscroll, fadeTop = 0, class: cls = '', children }: Props = $props();
	let more = $state({ t: false, b: false });
	function upd() {
		if (!el) return;
		top = el.scrollTop;
		more = { t: el.scrollTop > 2, b: el.scrollTop + el.clientHeight < el.scrollHeight - 2 };
	}
	$effect(() => {
		if (!el) return;
		const ro = new ResizeObserver(upd);
		ro.observe(el);
		if (el.firstElementChild) ro.observe(el.firstElementChild);
		upd();
		return () => ro.disconnect();
	});
</script>

<div class="relative min-h-0 flex-1 {cls}">
	<!-- svelte-ignore a11y_no_noninteractive_tabindex (keyboard users scroll a region that has more content) -->
	<div
		bind:this={el}
		bind:clientHeight={viewH}
		class="thin absolute inset-0 overflow-auto overscroll-contain [overflow-anchor:none]"
		onscroll={(e) => (upd(), onscroll?.(e))}
		tabindex={more.t || more.b ? 0 : -1}
	>
		<div class="min-h-full min-w-0">{@render children()}</div>
	</div>
	<!-- a crisp line, not a gradient: the old shadow read as a grey blur over the content -->
	{#if more.t}<div class="pointer-events-none absolute inset-x-0 z-20 border-t border-line" style="top:{fadeTop}px" aria-hidden="true"></div>{/if}
	{#if more.b}<div class="pointer-events-none absolute inset-x-0 bottom-0 z-20 border-t border-line" aria-hidden="true"></div>{/if}
</div>
