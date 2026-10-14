<!--
	Segmented — one choice out of a short, fixed list, shown all at once.

	Two shapes, both already in the routes:

	  joined  the grid/table view toggle in /collection and /sealed — glyph
	          buttons welded into one control, shared rule, end caps rounded
	  pill    /collection's grid sort row — separate pills with a gap, one
	          width apiece so eight labels from "#" to "Rarity" read as peers
	          rather than as a ragged line

	The active segment is a wash and an accent edge, never a solid slab: a
	crimson block beside 4,763 pieces of card art was the loudest thing on the
	page, and the whole job of that page is the art.

	`onselect` fires on EVERY activation, including a click on the already
	active segment. /collection's sort row depends on it — clicking the active
	field flips asc/desc rather than doing nothing.

	Semantics are `role="group"` + `aria-pressed`, matching what the routes
	already ship: a row of toggle buttons, each individually tabbable. Not a
	radiogroup, which would ask for arrow-key roving focus the routes have
	never had.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Item = {
		value: string;
		/** What the segment reads as — a word, or a glyph like ▦ / ≡. */
		label: string;
		/** Accessible name; required when `label` is a glyph. */
		ariaLabel?: string;
		title?: string;
		/** UI-intent hook, e.g. `view-grid`. */
		testid?: string;
		disabled?: boolean;
	};

	type Props = {
		items: Item[];
		/** The selected `value`. Nothing matches → nothing is pressed. */
		value?: string;
		/** The group's accessible name — "View", "Sort". */
		label: string;
		variant?: 'joined' | 'pill';
		size?: 'sm' | 'md' | 'lg';
		/** Every segment takes the same width, so the row reads as one control. */
		equal?: boolean;
		/** Fires on every click, including on the active segment. */
		onselect?: (value: string) => void;
		class?: string;
		/** Rendered inside each segment after its label, given the item and
		    whether it is the selected one — /collection's asc/desc caret. */
		adornment?: Snippet<[Item, boolean]>;
	} & HTMLAttributes<HTMLDivElement>;

	let {
		items,
		value = $bindable(),
		label,
		variant = 'joined',
		size = 'md',
		equal = false,
		onselect = undefined,
		class: extra = '',
		adornment,
		...rest
	}: Props = $props();

	const classes = $derived(
		['segmented', variant, `s-${size}`, equal && 'equal', extra].filter(Boolean).join(' ')
	);

	/** @param {Item} item */
	function select(item: Item) {
		value = item.value;
		onselect?.(item.value);
	}
</script>

<div class={classes} role="group" aria-label={label} {...rest}>
	{#each items as item (item.value)}
		<button
			class="segment"
			class:on={item.value === value}
			type="button"
			aria-pressed={item.value === value}
			aria-label={item.ariaLabel}
			title={item.title}
			data-testid={item.testid}
			disabled={item.disabled}
			onclick={() => select(item)}
		>
			{item.label}
			{@render adornment?.(item, item.value === value)}
		</button>
	{/each}
</div>

<style>
	.segmented {
		/* The floor a segment widens to under `equal`. Component-local: it is
		   this control's own geometry, not a step any other component shares. */
		--segment-min-width: 5.5rem;
		display: flex;
		align-items: center;
	}
	.pill {
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.segment {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: var(--space-1);
		background: none;
		border: 1px solid var(--color-border);
		color: var(--color-text-subtle);
		font: inherit;
		line-height: var(--leading-tight);
		white-space: nowrap;
		cursor: pointer;
		transition:
			background-color var(--dur-fast) var(--ease-standard),
			border-color var(--dur-fast) var(--ease-standard),
			color var(--dur-fast) var(--ease-standard);
	}
	.segment:hover:not(:disabled) {
		border-color: var(--color-border-accent);
		color: var(--color-text);
	}
	.segment:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
		/* Lift the ring over the neighbour it shares an edge with. */
		position: relative;
		z-index: 1;
	}
	.segment:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.equal .segment {
		min-width: var(--segment-min-width);
	}

	/* --- size -------------------------------------------------------------- */
	.s-sm .segment {
		padding: var(--space-1) var(--space-2);
		font-size: var(--text-sm);
	}
	.s-md .segment {
		padding: var(--space-1) var(--space-3);
		font-size: var(--text-md);
	}
	/* `lg` is the glyph size: a ▦ needs the type scale, not more padding. */
	.s-lg .segment {
		padding: var(--space-1) var(--space-2);
		font-size: var(--text-xl);
	}

	/* --- variant: joined ---------------------------------------------------
	   Welded: neighbours share one rule, and only the two ends are rounded.
	   Written as corner longhands rather than a `radius 0 0 radius` shorthand
	   so no raw dimension has to appear to say "square this corner". */
	.joined .segment:not(:first-child) {
		border-left: none;
	}
	.joined .segment:first-child {
		border-start-start-radius: var(--radius-md);
		border-end-start-radius: var(--radius-md);
	}
	.joined .segment:last-child {
		border-start-end-radius: var(--radius-md);
		border-end-end-radius: var(--radius-md);
	}
	.joined .segment.on {
		background: var(--color-info-surface);
		color: var(--color-text);
	}

	/* --- variant: pill ----------------------------------------------------- */
	.pill .segment {
		border-color: var(--color-border-subtle);
		border-radius: var(--radius-pill);
	}
	.pill .segment.on {
		background: var(--color-surface-selected);
		border-color: var(--color-border-accent);
		color: var(--color-text-accent);
	}
</style>
