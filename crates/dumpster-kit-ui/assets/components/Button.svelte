<!--
	Button — every clickable control in the app, in five shapes that already
	exist in the routes:

	  primary    the crimson call to action: "Create binder", "Add", "Import"
	  secondary  the flat blue action row in /ingest/csv ("Preview", "Commit")
	  ghost      the bordered panel-coloured control: browse's view toggles,
	             /ingest/csv's selection tools, /collection's sort button
	  danger     destructive: "Delete deck", "Delete binder"
	  link       the unstyled inline action ("cancel", "clear") that routes
	             wrote as `button.link { background: none; padding: 0 }`

	`href` renders an anchor with identical styling — /orders' "Import order"
	is a link that has always been painted as a primary button.

	Note the primary fill: it is exactly the historical #e94560, because the
	token layer moved the CONTRAST fix onto the label (`--color-on-accent`, dark
	ink at 5.02:1) instead of onto the brand colour. Do not "fix" it back.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAnchorAttributes, HTMLButtonAttributes } from 'svelte/elements';

	type Props = {
		variant?: 'primary' | 'secondary' | 'ghost' | 'danger' | 'link';
		size?: 'sm' | 'md' | 'lg';
		/** Stretches to the container — the full-width submit in a narrow form. */
		block?: boolean;
		href?: string;
		disabled?: boolean;
		type?: 'button' | 'submit' | 'reset';
		class?: string;
		children: Snippet;
	} & Omit<HTMLButtonAttributes & HTMLAnchorAttributes, 'type'>;

	let {
		variant = 'primary',
		size = 'md',
		block = false,
		href = undefined,
		disabled = false,
		type = 'button',
		class: extra = '',
		children,
		...rest
	}: Props = $props();

	const classes = $derived(
		['btn', variant, `s-${size}`, block && 'block', extra].filter(Boolean).join(' ')
	);
</script>

{#if href}
	<!-- A disabled link is not a thing in HTML; mark it and drop the target so
	     keyboard users get the same answer as mouse users. -->
	<a
		class={classes}
		href={disabled ? undefined : href}
		aria-disabled={disabled ? 'true' : undefined}
		tabindex={disabled ? -1 : undefined}
		{...rest}>{@render children()}</a
	>
{:else}
	<button class={classes} {type} {disabled} {...rest}>{@render children()}</button>
{/if}

<style>
	.btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: var(--space-2);
		border: 1px solid transparent;
		border-radius: var(--radius-md);
		font: inherit;
		font-weight: var(--weight-medium);
		line-height: var(--leading-tight);
		text-decoration: none;
		white-space: nowrap;
		cursor: pointer;
		transition:
			background-color var(--dur-fast) var(--ease-standard),
			border-color var(--dur-fast) var(--ease-standard),
			color var(--dur-fast) var(--ease-standard);
	}
	.btn:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
	.btn:disabled,
	.btn[aria-disabled='true'] {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.block {
		display: flex;
		width: 100%;
	}

	/* --- size ------------------------------------------------------------- */
	.s-sm {
		padding: var(--space-1) var(--space-2);
		font-size: var(--text-sm);
	}
	.s-md {
		padding: var(--space-2) var(--space-3);
		font-size: var(--text-lg);
	}
	.s-lg {
		padding: var(--space-2) var(--space-4);
		font-size: var(--text-xl);
	}
	.link.s-sm,
	.link.s-md,
	.link.s-lg {
		padding: var(--space-0);
	}

	/* --- variant ---------------------------------------------------------- */
	.primary {
		background: var(--color-accent);
		color: var(--color-on-accent);
	}
	.primary:hover:not(:disabled):not([aria-disabled='true']) {
		background: var(--color-accent-hover);
	}
	.primary:active:not(:disabled) {
		background: var(--color-accent-active);
	}

	.secondary {
		background: var(--color-info-surface);
		border-color: var(--color-border);
		color: var(--color-text);
	}
	.secondary:hover:not(:disabled):not([aria-disabled='true']) {
		border-color: var(--color-border-accent);
		color: var(--color-text-strong);
	}

	.ghost {
		background: var(--color-surface-panel);
		border-color: var(--color-border);
		color: var(--color-text-muted);
	}
	.ghost:hover:not(:disabled):not([aria-disabled='true']) {
		border-color: var(--color-border-accent);
		color: var(--color-text-strong);
	}

	.danger {
		background: var(--color-danger);
		color: var(--color-on-danger);
	}
	.danger:hover:not(:disabled):not([aria-disabled='true']) {
		background: var(--color-danger-hover);
	}

	.link {
		background: none;
		border-color: transparent;
		color: var(--color-text-subtle);
	}
	.link:hover:not(:disabled):not([aria-disabled='true']) {
		color: var(--color-text-accent);
	}
</style>
