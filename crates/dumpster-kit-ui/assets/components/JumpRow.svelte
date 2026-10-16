<!--
	JumpRow — the "jump to" chip row at the top of a browse page.

	It wraps. A row of chips that scrolls sideways is how /browse came to
	overflow a 390px phone, so the row is a wrapping flex box and each chip may
	shrink below its text and break the label. Chips are buttons; the caller
	owns what a click does (open the section, scroll to it).
-->
<script lang="ts">
	import type { HTMLAttributes } from 'svelte/elements';

	type Item = {
		key: string;
		label: string;
		/** Trailing count — "12" sets in the series. */
		count?: number | string;
		/** Marks a chip that leads somewhere the user has cards in. */
		active?: boolean;
	};

	type Props = {
		items: Item[];
		label?: string;
		onjump?: (key: string) => void;
		class?: string;
	} & Omit<HTMLAttributes<HTMLElement>, 'onjump'>;

	let { items, label = 'Jump to', onjump, class: extra = '', ...rest }: Props = $props();
</script>

<nav class={['jump', extra].filter(Boolean).join(' ')} aria-label={label} {...rest}>
	<span class="label">{label}</span>
	{#each items as item (item.key)}
		<button
			type="button"
			class="chip"
			class:active={item.active}
			onclick={() => onjump?.(item.key)}
		>
			<span class="name">{item.label}</span>
			{#if item.count !== undefined}<span class="count">{item.count}</span>{/if}
		</button>
	{/each}
</nav>

<style>
	.jump {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
		min-width: 0;
		max-width: 100%;
	}
	.label {
		font-size: var(--text-sm);
		color: var(--color-text-subtle);
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}
	.chip {
		display: inline-flex;
		align-items: baseline;
		gap: var(--space-1);
		min-width: 0;
		max-width: 100%;
		padding: var(--space-1) var(--space-3);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-pill);
		background: var(--color-surface-panel);
		color: var(--color-text);
		font: inherit;
		font-size: var(--text-sm);
		text-align: left;
		cursor: pointer;
	}
	.chip.active {
		border-color: var(--color-border-accent);
	}
	.chip:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
	.name {
		min-width: 0;
		overflow-wrap: anywhere;
	}
	.count {
		flex: none;
		color: var(--color-text-subtle);
	}
</style>
