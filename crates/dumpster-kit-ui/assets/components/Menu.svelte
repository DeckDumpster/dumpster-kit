<!--
	Menu — the ⋯ burger and the popover it opens.

	/collection and /sealed both ship one, and both ship the same five parts:
	a ⋯ trigger, a fixed backdrop that swallows the next click anywhere else, a
	popover anchored to the trigger's own edge, rows that are anchors when they
	download something and buttons when they do something, and a close on every
	one of those.

	Rows are data, not markup: `{ label, href | onclick }` is exactly what both
	routes' menus already are, and passing them as an array is what lets this
	own the parts a route kept getting wrong — closing after a choice, closing
	on Escape, closing on an outside click. The `children` snippet is there for
	a row the array cannot express; a route that reaches for it for STYLE has
	found a variant this component is missing.

	`align="end"` hangs the popover off the trigger's right edge, which is what
	a trigger riding the right-hand end of a bar needs — a left-anchored menu
	there runs off a narrow viewport.

	The surface is Panel `overlay` + elevation, so the popover fill, rule and
	radius stay one decision made in one place; this component contributes
	only where the box sits.

	The ⋯ itself is a plain button rather than a Button `link`: it is a part of
	this control, not a control a route chose, and it is sized by its glyph.
	Same reasoning as SearchField's ×. A route never styles either.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';
	import Panel from './Panel.svelte';

	type Item = {
		label: string;
		/** Renders the row as a link — every export row in the app. */
		href?: string;
		/** `download` on that link. */
		download?: boolean;
		onclick?: () => void;
		disabled?: boolean;
		/** UI-intent hook. */
		testid?: string;
		/** Destructive rows read on the danger ramp, never the brand one. */
		tone?: 'default' | 'danger';
	};

	type Props = {
		items?: Item[];
		/** Bindable, for a route that closes the menu from elsewhere. */
		open?: boolean;
		/** The trigger's accessible name. */
		label?: string;
		/** Which edge of the trigger the popover hangs from. */
		align?: 'start' | 'end';
		/** How wide the popover opens before its rows widen it further. */
		width?: 'sm' | 'md';
		/** Replaces the ⋯ glyph. */
		trigger?: Snippet;
		/** Rows the `items` shape cannot express. Rendered after them. */
		children?: Snippet;
		class?: string;
	} & HTMLAttributes<HTMLDivElement>;

	let {
		items = [],
		open = $bindable(false),
		label = 'Menu',
		align = 'end',
		width = 'md',
		trigger,
		children,
		class: extra = '',
		...rest
	}: Props = $props();

	const classes = $derived(['menuwrap', extra].filter(Boolean).join(' '));

	function close() {
		open = false;
	}

	/** @param {Item} item */
	function choose(item: Item) {
		// Close first: an `href` row is about to navigate or download, and a
		// menu still standing over the page afterwards is the bug both routes
		// hand-rolled a `closeMenu()` call into every row to avoid.
		close();
		item.onclick?.();
	}
</script>

<svelte:window
	onkeydown={(e) => {
		if (open && e.key === 'Escape') close();
	}}
/>

<div class={classes} {...rest}>
	<button
		class="trigger"
		type="button"
		aria-label={label}
		title={label}
		aria-haspopup="true"
		aria-expanded={open}
		onclick={() => (open = !open)}
	>
		{#if trigger}{@render trigger()}{:else}⋯{/if}
	</button>

	{#if open}
		<Panel variant="overlay" elevation="md" padding="sm" class="pop a-{align} w-{width}" role="menu">
			{#each items as item (item.label)}
				{#if item.href}
					<a
						class="item t-{item.tone ?? 'default'}"
						href={item.href}
						download={item.download}
						role="menuitem"
						data-testid={item.testid}
						onclick={() => choose(item)}>{item.label}</a
					>
				{:else}
					<button
						class="item t-{item.tone ?? 'default'}"
						type="button"
						role="menuitem"
						data-testid={item.testid}
						disabled={item.disabled}
						onclick={() => choose(item)}>{item.label}</button
					>
				{/if}
			{/each}
			{@render children?.()}
		</Panel>
	{/if}
</div>

{#if open}
	<!-- Sits under the popover and over everything else, so the next click
	     anywhere on the page closes the menu instead of doing two things. -->
	<div class="backdrop" role="presentation" onclick={close}></div>
{/if}

<style>
	.menuwrap {
		position: relative;
		display: inline-flex;
	}

	.trigger {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: var(--space-1) var(--space-2);
		background: none;
		border: none;
		border-radius: var(--radius-sm);
		color: var(--color-text-subtle);
		font: inherit;
		font-size: var(--text-2xl);
		line-height: var(--leading-tight);
		cursor: pointer;
	}
	.trigger:hover {
		color: var(--color-text-accent);
	}
	.trigger:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}

	/* Placement only — Panel `overlay` paints the box. The rule is written as
	   `:global()` under a scoped ancestor because the class lands on markup
	   this file does not own; a bare `:global(.pop)` would leak to every
	   route. */
	.menuwrap :global(.pop) {
		position: absolute;
		top: calc(100% + var(--space-1));
		/* Above the sticky bar it drops out of, below a modal and its scrim. */
		z-index: 60;
		display: flex;
		flex-direction: column;
	}
	.menuwrap :global(.pop.a-start) {
		left: 0;
	}
	.menuwrap :global(.pop.a-end) {
		right: 0;
	}
	/* Wide enough that the labels are the width, not a round number:
	   "Export sealed (Collectr)" at sm, "Export JSON (full backup)" at md. */
	.menuwrap :global(.pop.w-sm) {
		min-width: 12.5rem;
	}
	.menuwrap :global(.pop.w-md) {
		min-width: 15rem;
	}

	.item {
		display: block;
		width: 100%;
		padding: var(--space-2) var(--space-3);
		background: none;
		border: none;
		border-radius: var(--radius-sm);
		color: var(--color-text);
		font: inherit;
		font-size: var(--text-lg);
		text-align: left;
		text-decoration: none;
		white-space: nowrap;
		cursor: pointer;
	}
	.item:hover {
		background: var(--color-surface-hover);
		color: var(--color-text-accent);
	}
	.item:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
	.item:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.t-danger {
		color: var(--color-danger-text);
	}
	.t-danger:hover {
		background: var(--color-danger-surface);
		color: var(--color-danger-text);
	}

	.backdrop {
		position: fixed;
		inset: 0;
		/* Over the page, but UNDER a sticky Toolbar (z-index 50) — which is
		   where both burgers live. A backdrop that covered the bar would eat
		   the first click on every other control in it, so "click away to
		   close" would cost two clicks. */
		z-index: 49;
	}
</style>
