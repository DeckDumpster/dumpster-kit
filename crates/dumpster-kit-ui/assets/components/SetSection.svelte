<!--
	SetSection — a collapsible category section on a browse page: a toggle
	header (caret, title, counts) over a body.

	The title and the counts both wrap. A long series name beside a
	"12 sets · 1,204 / 3,000 cards (40%)" line is wider than a phone, and the
	header used to be a nowrap flex row that pushed the page sideways. Here the
	header is a wrapping flex row whose children may shrink and break anywhere.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		title: string;
		/** Secondary text beside the title — set count, owned/total cards. */
		meta?: string;
		open?: boolean;
		class?: string;
		children: Snippet;
	} & HTMLAttributes<HTMLElement>;

	let {
		title,
		meta = undefined,
		open = $bindable(true),
		class: extra = '',
		children,
		...rest
	}: Props = $props();
</script>

<section class={['section', extra].filter(Boolean).join(' ')} {...rest}>
	<button type="button" class="head" aria-expanded={open} onclick={() => (open = !open)}>
		<span class="caret" aria-hidden="true">{open ? '▾' : '▸'}</span>
		<span class="title">{title}</span>
		{#if meta}<span class="meta">{meta}</span>{/if}
	</button>
	{#if open}<div class="body">{@render children()}</div>{/if}
</section>

<style>
	.section {
		min-width: 0;
		max-width: 100%;
	}
	.head {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-1) var(--space-3);
		width: 100%;
		min-width: 0;
		padding: var(--space-2) var(--space-0);
		border: 0;
		border-bottom: 1px solid var(--color-border);
		background: none;
		color: var(--color-text-strong);
		font: inherit;
		text-align: left;
		cursor: pointer;
	}
	.head:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
	.caret {
		flex: none;
	}
	.title {
		min-width: 0;
		font-size: var(--text-xl);
		font-weight: var(--weight-semibold);
		line-height: var(--leading-tight);
		overflow-wrap: anywhere;
	}
	.meta {
		min-width: 0;
		font-size: var(--text-sm);
		color: var(--color-text-subtle);
		overflow-wrap: anywhere;
	}
	.body {
		min-width: 0;
		padding-top: var(--space-3);
	}
</style>
