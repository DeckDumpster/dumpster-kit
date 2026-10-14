<!--
	Badge — the small status marker, in the two shapes the app already uses:

	  pill  a rounded chip: the amber count on the home page, the "sealed"
	        marker in /ingest/csv, MatchPicker's confidence chips
	  tag   the square, upper-cased micro-label /collection and /sealed stamp
	        beside a row's name (condition, status, provenance)

	and in three weights: `solid` (filled with the tone), `soft` (tone's dark
	surface + tone's text + tone's rule — /ingest/csv's warning badge) and
	`outline` (rule and text only).

	Tone maps to a state, never to a decoration. Each tone resolves the four
	roles it needs into component-local properties, so the weight rules below
	are written once instead of six times — and every colour still arrives
	through a semantic token.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		tone?: 'neutral' | 'accent' | 'success' | 'warning' | 'danger' | 'info';
		variant?: 'solid' | 'soft' | 'outline';
		shape?: 'pill' | 'tag';
		size?: 'sm' | 'md';
		class?: string;
		children: Snippet;
	} & HTMLAttributes<HTMLSpanElement>;

	let {
		tone = 'neutral',
		variant = 'soft',
		shape = 'pill',
		size = 'md',
		class: extra = '',
		children,
		...rest
	}: Props = $props();

	const classes = $derived(
		['badge', `t-${tone}`, variant, shape, `s-${size}`, extra].filter(Boolean).join(' ')
	);
</script>

<span class={classes} {...rest}>{@render children()}</span>

<style>
	.badge {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		border: 1px solid transparent;
		font-weight: var(--weight-semibold);
		line-height: var(--leading-tight);
		vertical-align: middle;
		white-space: nowrap;
	}

	/* --- tone: resolve the four roles this tone answers with --------------- */
	.t-neutral {
		--badge-fill: var(--color-neutral);
		--badge-on-fill: var(--color-neutral-text);
		--badge-surface: var(--color-neutral-surface);
		--badge-text: var(--color-neutral-text);
		--badge-border: var(--color-neutral-border);
	}
	.t-accent {
		--badge-fill: var(--color-accent);
		--badge-on-fill: var(--color-on-accent);
		--badge-surface: var(--color-accent-surface);
		--badge-text: var(--color-text-accent);
		--badge-border: var(--color-border-accent);
	}
	.t-success {
		--badge-fill: var(--color-success);
		--badge-on-fill: var(--color-on-success);
		--badge-surface: var(--color-success-surface);
		--badge-text: var(--color-success-text);
		--badge-border: var(--color-success-border);
	}
	.t-warning {
		--badge-fill: var(--color-warning);
		--badge-on-fill: var(--color-on-warning);
		--badge-surface: var(--color-warning-surface);
		--badge-text: var(--color-warning-text);
		--badge-border: var(--color-warning-border);
	}
	.t-danger {
		--badge-fill: var(--color-danger);
		--badge-on-fill: var(--color-on-danger);
		--badge-surface: var(--color-danger-surface);
		--badge-text: var(--color-danger-text);
		--badge-border: var(--color-danger-border);
	}
	.t-info {
		--badge-fill: var(--color-info);
		--badge-on-fill: var(--color-on-info);
		--badge-surface: var(--color-info-surface);
		--badge-text: var(--color-info-text);
		--badge-border: var(--color-info-border);
	}

	/* --- weight ----------------------------------------------------------- */
	.solid {
		background: var(--badge-fill);
		border-color: var(--badge-fill);
		color: var(--badge-on-fill);
	}
	.soft {
		background: var(--badge-surface);
		border-color: var(--badge-border);
		color: var(--badge-text);
	}
	.outline {
		background: none;
		border-color: var(--badge-border);
		color: var(--badge-text);
	}

	/* --- shape / size ----------------------------------------------------- */
	.pill {
		border-radius: var(--radius-pill);
		padding: var(--space-0-5) var(--space-2);
	}
	.tag {
		border-radius: var(--radius-xs);
		padding: var(--space-px) var(--space-1);
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}
	.s-sm {
		font-size: var(--text-xs);
	}
	.s-md {
		font-size: var(--text-sm);
	}
</style>
