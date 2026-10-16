<!--
	TileGrid — the set-tile grid inside a browse section.

	Columns are `repeat(auto-fill, minmax(min(100%, 14rem), 1fr))`: the `min()`
	is what keeps a single column from being wider than a 390px viewport (a bare
	`minmax(14rem, 1fr)` track cannot shrink below 14rem and overflows once the
	page padding is taken out). Grid items get `min-width: 0` so a long set name
	wraps inside its tile instead of stretching the track.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		class?: string;
		children: Snippet;
	} & HTMLAttributes<HTMLDivElement>;

	let { class: extra = '', children, ...rest }: Props = $props();
</script>

<div class={['grid', extra].filter(Boolean).join(' ')} {...rest}>{@render children()}</div>

<style>
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 14rem), 1fr));
		gap: var(--space-3);
		min-width: 0;
		max-width: 100%;
	}
	.grid > :global(*) {
		min-width: 0;
		overflow-wrap: anywhere;
	}
</style>
