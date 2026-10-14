<!--
	Panel — the bordered surface the app already draws by hand in a dozen places.

	Derived from the real usages, not from a design system in general:
	  · the stats/summary card      (panel fill, 1px rule, radius, roomy pad)
	  · the browse / binders / decks tile  (same box, but a link, and its
	    border goes accent on hover — `interactive` + `href`)
	  · the popovers: sort menu, autocomplete list, browse's set nav
	    (`elevation="md"`, tight pad)
	  · modal bodies (`variant="overlay"`)

	Everything a route used to restate — background, rule, radius, hover — is
	settled here, so a route only says which of those four things it is.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		/** Which surface in the elevation stack this box sits on. */
		variant?: 'panel' | 'raised' | 'sunken' | 'overlay';
		padding?: 'none' | 'sm' | 'md' | 'lg';
		/** The border answers to hover and focus. Implied by `href`. */
		interactive?: boolean;
		/** Renders an anchor instead of a div — the tile in /browse, /binders. */
		href?: string;
		elevation?: 'none' | 'sm' | 'md' | 'lg';
		class?: string;
		children: Snippet;
	} & HTMLAttributes<HTMLElement>;

	let {
		variant = 'panel',
		padding = 'md',
		interactive = false,
		href = undefined,
		elevation = 'none',
		class: extra = '',
		children,
		...rest
	}: Props = $props();

	// A panel with an href is a link whether or not the caller said
	// `interactive`; the hover affordance is not optional on something clickable.
	const clickable = $derived(interactive || href !== undefined);
	const classes = $derived(
		['panel', `v-${variant}`, `p-${padding}`, `e-${elevation}`, clickable && 'interactive', extra]
			.filter(Boolean)
			.join(' ')
	);
</script>

{#if href}
	<a {href} class={classes} {...rest}>{@render children()}</a>
{:else}
	<div class={classes} {...rest}>{@render children()}</div>
{/if}

<style>
	.panel {
		display: block;
		border: 1px solid var(--color-border);
		border-radius: var(--radius-lg);
		color: var(--color-text);
		background: var(--color-surface-panel);
	}

	/* --- surface ---------------------------------------------------------- */
	.v-raised {
		background: var(--color-surface-raised);
	}
	.v-sunken {
		background: var(--color-surface-sunken);
		border-color: var(--color-border-subtle);
	}
	.v-overlay {
		background: var(--color-surface-overlay);
	}

	/* --- padding ---------------------------------------------------------- */
	.p-none {
		padding: var(--space-0);
	}
	.p-sm {
		padding: var(--space-2);
	}
	.p-md {
		padding: var(--space-4);
	}
	.p-lg {
		padding: var(--space-5) var(--space-6);
	}

	/* --- elevation -------------------------------------------------------- */
	.e-sm {
		box-shadow: var(--shadow-sm);
	}
	.e-md {
		box-shadow: var(--shadow-md);
	}
	.e-lg {
		box-shadow: var(--shadow-lg);
	}

	/* --- interactive ------------------------------------------------------ */
	.interactive {
		text-decoration: none;
		transition: border-color var(--dur-base) var(--ease-standard);
	}
	.interactive:hover {
		border-color: var(--color-border-accent);
	}
	.interactive:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
</style>
