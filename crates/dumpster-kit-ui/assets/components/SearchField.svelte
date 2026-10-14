<!--
	SearchField — the filter box that leads a page's top bar.

	Four routes wrote it by hand and wrote it the same way each time: a
	relative wrapper, a text input carrying the control fill/rule/radius, extra
	right padding, and an absolutely positioned × that appears once there is
	something to clear (/collection, /sealed, /browse, /browse/[set],
	CollectionPicker). The only thing that genuinely differed between them was
	how wide the box is allowed to get — so that is the one thing this takes as
	a variant.

	It is NOT `Field type="text"`. Field is a labelled column: caption above,
	hint below, control sized by its content. This is a single control that
	claims a flex slot in a toolbar, hangs a clear button inside its own box,
	and anchors a popover to it. Modelling it as a Field variant would mean
	four props that only apply when `search` is set.

	`invalid` is a boolean rather than Field's `error` string: there is nowhere
	inside a one-line control to put the message. /collection prints the parse
	error under the bar as its own line and reddens the boundary through this.

	The `children` snippet renders inside the wrapper, which is the positioning
	context — that is where /collection's autocomplete listbox hangs, and why
	the wrapper (not the input) takes `class`.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLInputAttributes } from 'svelte/elements';

	type Props = {
		value?: string;
		placeholder?: string;
		/** The control's accessible name. A search box rarely has a visible one. */
		label?: string;
		/** How much of the row the box may claim. See the `w-*` rules below. */
		width?: 'compact' | 'comfortable' | 'full';
		/** The × that empties the box, shown once there is something to empty. */
		clearable?: boolean;
		/** Reddens the boundary; the message goes wherever the route has room. */
		invalid?: boolean;
		disabled?: boolean;
		/** Called after the × empties the box — for routes that drive `value`
		    one-way through `oninput` rather than binding it. */
		onclear?: () => void;
		/** The input element, for a route that needs to focus or measure it. */
		element?: HTMLInputElement;
		class?: string;
		/** A popover anchored to the box — /collection's query autocomplete. */
		children?: Snippet;
		// placeholder, autocomplete, oninput, onkeydown, role, aria-expanded and
		// data-testid all reach the INPUT through `...rest`.
	} & Omit<HTMLInputAttributes, 'value' | 'size' | 'type'>;

	let {
		value = $bindable(''),
		placeholder = 'Search…',
		label = undefined,
		width = 'comfortable',
		clearable = true,
		invalid = false,
		disabled = false,
		onclear = undefined,
		element = $bindable(),
		class: extra = '',
		children,
		...rest
	}: Props = $props();

	const classes = $derived(['searchfield', `w-${width}`, extra].filter(Boolean).join(' '));

	function clear() {
		value = '';
		onclear?.();
		// Emptying a box you are typing in should leave you typing in it.
		element?.focus();
	}
</script>

<div class={classes}>
	<input
		class="input"
		class:invalid
		type="text"
		autocomplete="off"
		aria-label={label}
		{placeholder}
		{disabled}
		bind:value
		bind:this={element}
		{...rest}
	/>
	{#if clearable && value}
		<button class="clear" type="button" aria-label="Clear search" title="Clear" onclick={clear}
			>×</button
		>
	{/if}
	{@render children?.()}
</div>

<style>
	.searchfield {
		position: relative;
		display: flex;
		align-items: center;
		min-width: 0;
	}

	/* --- width ------------------------------------------------------------
	   All three grow into the row; they differ in how far they are allowed to
	   run before the row's other controls get the rest. `full` is the bar that
	   is mostly search (/sealed); `comfortable` still leads its bar but stops
	   short of a 1920-wide trough no query ever fills (/collection); `compact`
	   sits among peers and yields to them (/browse). */
	.w-compact {
		flex: 1 1 14rem;
		max-width: 22.5rem;
	}
	.w-comfortable {
		flex: 1 1 22rem;
		max-width: 44rem;
	}
	.w-full {
		flex: 1 1 auto;
	}

	.input {
		flex: 1;
		min-width: 0;
		/* Right pad clears the × so it never sits on top of typed text. */
		padding: var(--space-2) var(--space-8) var(--space-2) var(--space-3);
		background: var(--color-control-surface);
		border: 1px solid var(--color-control-border);
		border-radius: var(--radius-md);
		color: var(--color-control-text);
		font: inherit;
	}
	.input::placeholder {
		color: var(--color-control-placeholder);
	}
	.input:focus-visible {
		outline: none;
		border-color: var(--color-border-focus);
		box-shadow: var(--shadow-focus);
	}
	.input:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}
	/* An unparseable query reddens on the danger ramp, not the brand one —
	   the same answer Field gives for `error`. */
	.input.invalid {
		border-color: var(--color-danger);
	}

	.clear {
		position: absolute;
		right: var(--space-2);
		top: 50%;
		transform: translateY(-50%);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: var(--space-6);
		height: var(--space-6);
		padding: var(--space-0);
		background: none;
		border: none;
		border-radius: var(--radius-round);
		color: var(--color-text-subtle);
		font-size: var(--text-xl);
		line-height: var(--leading-tight);
		cursor: pointer;
	}
	.clear:hover {
		background: var(--color-surface-selected);
		color: var(--color-text-accent);
	}
	.clear:focus-visible {
		outline: none;
		box-shadow: var(--shadow-focus);
	}
</style>
