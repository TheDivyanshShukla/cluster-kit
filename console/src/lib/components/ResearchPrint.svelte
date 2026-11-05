<script lang="ts">
	import type { Snippet } from 'svelte';
	// Wraps what a page prints: in the print preview everything outside it (menus, headers, other panels) disappears,
	// its parents lose their fixed heights and scrolling, and the page is A4 on a white background.
	let { class: cls = '', children }: { class?: string; children: Snippet } = $props();
</script>

<svelte:head>{@html '<style>@page{size:A4;margin:12mm}</style>'}</svelte:head>

<div class="print-area {cls}">{@render children()}</div>

<style>
	@media print {
		:global(:root) {
			color-scheme: light !important;
		}
		:global(body :not(.print-area):not(:has(.print-area)):not(.print-area *)) {
			display: none !important;
		}
		:global(html, body, :has(> .print-area), :has(.print-area)) {
			display: block !important;
			height: auto !important;
			min-height: 0 !important;
			max-height: none !important;
			overflow: visible !important;
			position: static !important;
			padding: 0 !important;
			margin: 0 !important;
			border: 0 !important;
			width: auto !important;
			max-width: none !important;
			background: white !important;
		}
		:global(.print-area, .print-area *) {
			overflow: visible !important;
		}
		:global(.print-area .truncate) {
			overflow: hidden !important;
		}
		:global(.print-area .no-print) {
			display: none !important;
		}
		:global(.print-area) {
			font-size: 11px;
			color: black;
			break-inside: avoid;
		}
	}
</style>
