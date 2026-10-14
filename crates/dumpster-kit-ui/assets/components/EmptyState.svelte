<!--
	EmptyState — what a list says when it has nothing.

	Today every route says it as `<p class="muted">No decks yet. Create one
	above.</p>` — /binders, /decks, /orders, /batches, /recent, /wishlist,
	/ingest/unresolved, PriceChart, ValueHistoryChart, ManualPriceModal. Same
	sentence shape, eight different paddings.

	Two lines and an optional action, because that is what those messages
	already are: what is missing (`title`), why or what to do about it
	(`description`), and the control that fixes it. /ingest/unresolved's
	"Nothing unresolved — the queue is empty. 🎉" is the `tone="success"` case:
	empty is the good outcome there, not the sad one.

	Deliberately no illustration. Bead pd-0ksp designs these per route; this
	is the container that keeps them from drifting apart again.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		title: string;
		description?: string;
		/** `success` when empty is the desired state (an empty work queue). */
		tone?: 'neutral' | 'success';
		/** `sm` for an empty region inside a panel; `md` for a whole page. */
		size?: 'sm' | 'md';
		class?: string;
		/** The control that resolves the emptiness — usually a Button. */
		action?: Snippet;
	} & HTMLAttributes<HTMLDivElement>;

	let {
		title,
		description = undefined,
		tone = 'neutral',
		size = 'md',
		class: extra = '',
		action,
		...rest
	}: Props = $props();

	const classes = $derived(
		['empty', `t-${tone}`, `s-${size}`, extra].filter(Boolean).join(' ')
	);
</script>

<div class={classes} {...rest}>
	<p class="title">{title}</p>
	{#if description}<p class="description">{description}</p>{/if}
	{#if action}<div class="action">{@render action()}</div>{/if}
</div>

<style>
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-2);
		text-align: center;
	}
	/* `sm` is a sentence inside a box that already exists — a panel, a picker
	   list, a chart slot. It draws nothing of its own. */
	.s-sm {
		padding: var(--space-4) var(--space-3);
	}
	/* `md` IS the box. A page whose list is empty otherwise renders a heading,
	   a control and an ocean of page fill (pd-0ksp); the dashed well says
	   "content belongs here, and there is none yet" — which is the whole
	   message — without competing with the content that will replace it. */
	.s-md {
		padding: var(--space-10) var(--space-6);
		margin: var(--space-6) auto;
		max-width: 44rem;
		border: 1px dashed var(--color-border-subtle);
		border-radius: var(--radius-xl);
		background: var(--color-surface-well);
	}

	.title {
		margin: var(--space-0);
		font-size: var(--text-lg);
		font-weight: var(--weight-semibold);
		color: var(--color-text-muted);
	}
	.t-success .title {
		color: var(--color-success-text);
	}

	.description {
		margin: var(--space-0);
		max-width: 32rem;
		font-size: var(--text-md);
		line-height: var(--leading-normal);
		color: var(--color-text-subtle);
	}

	.action {
		margin-top: var(--space-2);
	}
</style>
