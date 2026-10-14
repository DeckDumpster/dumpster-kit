<!--
	ProgressBar — set-completion, the app's most repeated non-text figure.

	/browse draws one per set tile (and colours the base bar green against the
	master bar's crimson), /browse/[set] draws two in its header, and
	/browse/[set]/stats draws one per metric row at two thicknesses. All three
	hand-rolled the same track/fill pair.

	The fill's meaning is the tone, and the tone names a role rather than a
	colour: `complete` / `partial` / `empty` are the progress roles the token
	layer already declares, so a bar that later wants to colour itself BY
	completion changes one prop instead of three hex values.

	It is a real progressbar to assistive tech, which the hand-rolled
	`<div class="bar"><span style="width:40%">` never was.
-->
<script lang="ts">
	import type { HTMLAttributes } from 'svelte/elements';

	type Props = {
		value: number;
		max?: number;
		tone?: 'accent' | 'complete' | 'partial' | 'empty';
		size?: 'sm' | 'md';
		/** Caption above the track — "Base 42/102". Also the accessible name. */
		label?: string;
		/**
		 * Second half of the caption, pushed to the right edge and muted —
		 * "42 / 102 · 41%" against a plain "Base set" label. The stats page
		 * splits the figure off the name that way; without it that route
		 * would have to rebuild the caption row outside the primitive.
		 */
		hint?: string;
		/**
		 * Keep `label` as the accessible name but draw no caption. For a bar
		 * whose row already names it — the rarity-split table, where the
		 * tier is the cell two columns left — a visible caption would be a
		 * second copy of a label that is already on screen.
		 */
		labelHidden?: boolean;
		class?: string;
	} & HTMLAttributes<HTMLDivElement>;

	let {
		value,
		max = 100,
		tone = 'accent',
		size = 'sm',
		label = undefined,
		hint = undefined,
		labelHidden = false,
		class: extra = '',
		...rest
	}: Props = $props();

	// A set with no cards is 0%, not NaN% — /browse hits this on an
	// unpopulated synthesized set.
	const percent = $derived(max > 0 ? Math.min(100, Math.max(0, (value / max) * 100)) : 0);
	const classes = $derived(['bar', `t-${tone}`, `s-${size}`, extra].filter(Boolean).join(' '));
</script>

<div class={classes} {...rest}>
	{#if label && !labelHidden}
		<span class="label">
			<span>{label}</span>
			{#if hint}<span class="hint">{hint}</span>{/if}
		</span>
	{/if}
	<div
		class="track"
		role="progressbar"
		aria-valuenow={value}
		aria-valuemin={0}
		aria-valuemax={max}
		aria-label={label}
	>
		<span class="fill" style:width="{percent}%"></span>
	</div>
</div>

<style>
	.bar {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
	/* The caption is a row so `hint` can sit against the right edge. With
	   no hint it collapses to a plain left-aligned line — the shape
	   /browse and /browse/[set] already draw. */
	.label {
		display: flex;
		justify-content: space-between;
		gap: var(--space-2);
		font-size: var(--text-md);
		color: var(--color-text-muted);
	}
	.hint {
		color: var(--color-text-subtle);
	}
	.track {
		width: 100%;
		background: var(--color-progress-track);
		border-radius: var(--radius-pill);
		overflow: hidden;
	}
	.fill {
		display: block;
		height: 100%;
		background: var(--bar-fill);
		transition: width var(--dur-slow) var(--ease-standard);
	}

	/* --- size -------------------------------------------------------------
	   The hand-rolled bars were 6px and 8px; 6px is off the 4px ramp, so the
	   thin one snaps to --space-1. Snapping is the point of a ramp — the
	   alternative is a calc() that rebuilds an off-ramp value and re-opens
	   "every bar is its own thickness". */
	.s-sm .track {
		height: var(--space-1);
	}
	.s-md .track {
		height: var(--space-2);
	}

	/* --- tone ------------------------------------------------------------- */
	.t-accent {
		--bar-fill: var(--color-accent);
	}
	.t-complete {
		--bar-fill: var(--color-progress-fill);
	}
	.t-partial {
		--bar-fill: var(--color-progress-fill-partial);
	}
	.t-empty {
		--bar-fill: var(--color-progress-fill-empty);
	}
</style>
