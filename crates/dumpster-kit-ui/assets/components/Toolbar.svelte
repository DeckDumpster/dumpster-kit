<!--
	Toolbar — a row of controls.

	The routes call it `.newform`, `.controls`, `.seltools`, `.actions` and
	`.topbar`, and all five are `display: flex` with a gap and some subset of
	{align, wrap, sticky}. /collection and /sealed pin theirs to the top of the
	viewport over scrolling content, which is what `--color-surface-sticky`
	exists for — a translucent chrome that lets rows show through instead of
	stamping an opaque band across the page.

	A pinned bar that hosts a form control takes `surface="panel"` instead.
	WCAG 1.4.11 names the input-field boundary as something that must clear
	3:1, and `--color-control-border` manages only 2.61:1 against the sticky
	fill — the saturated blue is too light a ground for it. The panel surface
	sits a hair off the page colour, so it reads as a quieter band anyway.

	No visual weight of its own unless `sticky` or `bordered` says so: a
	toolbar is a layout, and chrome that recedes is the point.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		direction?: 'row' | 'column';
		gap?: 'sm' | 'md' | 'lg';
		/** `stretch` is what a `column` toolbar wants — rows fill the width. */
		align?: 'center' | 'baseline' | 'start' | 'end' | 'stretch';
		justify?: 'start' | 'between' | 'end';
		wrap?: boolean;
		/** Pins to the top of the scroll container over the content beneath. */
		sticky?: boolean;
		/** Fill for a `sticky` bar. `panel` is the opaque alternative for a bar
		    that hosts a form control — see the note at the top. Ignored when
		    the bar isn't pinned, which has no fill at all. */
		surface?: 'sticky' | 'panel';
		/** A rule under the row — implied by `sticky`. */
		bordered?: boolean;
		class?: string;
		children: Snippet;
	} & HTMLAttributes<HTMLDivElement>;

	let {
		direction = 'row',
		gap = 'md',
		align = 'center',
		justify = 'start',
		wrap = true,
		sticky = false,
		surface = 'sticky',
		bordered = false,
		class: extra = '',
		children,
		...rest
	}: Props = $props();

	const classes = $derived(
		[
			'toolbar',
			`d-${direction}`,
			`g-${gap}`,
			`a-${align}`,
			`j-${justify}`,
			wrap && 'wrap',
			sticky && 'sticky',
			sticky && `sf-${surface}`,
			(bordered || sticky) && 'bordered',
			extra
		]
			.filter(Boolean)
			.join(' ')
	);
</script>

<div class={classes} {...rest}>{@render children()}</div>

<style>
	.toolbar {
		display: flex;
	}
	.wrap {
		flex-wrap: wrap;
	}

	.d-row {
		flex-direction: row;
	}
	.d-column {
		flex-direction: column;
	}

	.g-sm {
		gap: var(--space-2);
	}
	.g-md {
		gap: var(--space-3);
	}
	.g-lg {
		gap: var(--space-4);
	}

	.a-center {
		align-items: center;
	}
	.a-baseline {
		align-items: baseline;
	}
	.a-start {
		align-items: flex-start;
	}
	.a-end {
		align-items: flex-end;
	}
	.a-stretch {
		align-items: stretch;
	}

	.j-start {
		justify-content: flex-start;
	}
	.j-between {
		justify-content: space-between;
	}
	.j-end {
		justify-content: flex-end;
	}

	.sticky {
		position: sticky;
		top: 0;
		/* Above the rows it scrolls over, below modals and their scrim. */
		z-index: 50;
		/* Roomier than the 6px/11px the routes hand-rolled. A pinned bar is
		   the one row of chrome always on screen; crowding it is what made
		   /collection's read as a strip of controls rather than a frame. */
		padding: var(--space-3) var(--space-4);
		background: var(--color-surface-sticky);
	}
	.sticky.sf-panel {
		background: var(--color-surface-panel);
	}
	.bordered {
		border-bottom: 1px solid var(--color-border);
	}
</style>
