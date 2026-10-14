<!--
	SectionHeader — the heading row that opens a section.

	Three shapes exist in the routes and they are the same object:
	  · /ingest/csv, /ingest/unresolved, /browse/[set]/stats all write
	    `h2 { font-size: .8rem; text-transform: uppercase; color: #888 }`,
	    sometimes with a 2px rule under it
	  · /ingest/csv's `h3` is the same header in amber, marking a warning group
	  · `.pane-head` is that header with a count and controls sharing its
	    baseline on the right

	So: a heading, an optional `meta` (the count), an optional `actions`
	snippet, an optional rule. `level` picks the heading element — an h2
	section inside an h1 page — and never has to agree with the size, which is
	the whole reason routes stopped using h-levels semantically.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		/** Heading text. Use `children` instead when it needs markup. */
		title?: string;
		level?: 1 | 2 | 3 | 4;
		/** Visual weight, independent of the heading level. */
		size?: 'sm' | 'md' | 'lg';
		tone?: 'subtle' | 'accent' | 'warning';
		/** Secondary text on the heading's baseline — "128 rows", a timestamp. */
		meta?: string;
		/** A rule under the header, as the ingest panes draw. */
		divider?: boolean;
		class?: string;
		children?: Snippet;
		/** Controls pinned to the right of the heading row. */
		actions?: Snippet;
	} & HTMLAttributes<HTMLElement>;

	let {
		title = undefined,
		level = 2,
		size = 'sm',
		tone = 'subtle',
		meta = undefined,
		divider = false,
		class: extra = '',
		children,
		actions,
		...rest
	}: Props = $props();

	const classes = $derived(
		['head', `s-${size}`, `t-${tone}`, divider && 'divider', extra].filter(Boolean).join(' ')
	);
</script>

<div class={classes} {...rest}>
	<svelte:element this={`h${level}`} class="title">
		{#if children}{@render children()}{:else}{title}{/if}
	</svelte:element>
	{#if meta}<span class="meta">{meta}</span>{/if}
	{#if actions}<div class="actions">{@render actions()}</div>{/if}
</div>

<style>
	.head {
		display: flex;
		align-items: baseline;
		gap: var(--space-3);
		margin: var(--space-5) var(--space-0) var(--space-2);
	}
	.divider {
		border-bottom: 1px solid var(--color-border);
		padding-bottom: var(--space-1);
	}

	.title {
		margin: var(--space-0);
		font-weight: var(--weight-semibold);
		line-height: var(--leading-tight);
	}

	/* --- size ------------------------------------------------------------- */
	.s-sm .title {
		font-size: var(--text-sm);
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}
	.s-md .title {
		font-size: var(--text-xl);
	}
	.s-lg .title {
		font-size: var(--text-2xl);
	}

	/* --- tone ------------------------------------------------------------- */
	.t-subtle .title {
		color: var(--color-text-subtle);
	}
	.t-accent .title {
		color: var(--color-text-accent);
	}
	.t-warning .title {
		color: var(--color-warning-text);
	}

	.meta {
		font-size: var(--text-sm);
		color: var(--color-text-subtle);
	}
	/* Pushed right so `actions` sits at the far end of the row whether or not
	   a meta is present. */
	.actions {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		margin-left: auto;
	}
</style>
