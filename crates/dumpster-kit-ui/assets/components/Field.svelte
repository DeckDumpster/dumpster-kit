<!--
	Field — a labelled form control.

	Every form in the app writes the same three lines by hand: a `<label>` laid
	out as a column with a 0.8rem muted caption, wrapping an input/select/
	textarea that carries the same fill, rule and radius (/binders, /decks,
	/wishlist, /ingest/manual, /ingest/order, /ingest/csv). Checkbox rows are
	the same thing turned on its side — that is `inline`.

	The control border is deliberately NOT the panel rule: WCAG 1.4.11 names
	the input boundary as a case that must clear 3:1, and the historical
	#0f3460 managed 1.27:1 against the panel. `--color-control-border` exists
	for exactly this, and is the one place the identity rule loses.

	The label wraps its control, so there is no id to thread through. Anything
	the control needs — placeholder, min, step, required, oninput — spreads
	through `...rest`. A control this doesn't cover (a combobox, a colour
	swatch row) goes in the `control` snippet and still gets the caption,
	hint and error treatment.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLInputAttributes } from 'svelte/elements';

	type Props = {
		label?: string;
		/** Which control to render. `control` overrides this entirely. */
		as?: 'input' | 'select' | 'textarea';
		type?: 'text' | 'number' | 'date' | 'checkbox' | 'radio' | 'file';
		/** Two-way for input/select/textarea; the option's value for a radio. */
		value?: string | number | undefined;
		checked?: boolean;
		/** The selected option of a radio set — `bind:group`, as HTML wants. */
		group?: string | number | undefined;
		/** Caption beside the control instead of above it — checkbox rows. */
		inline?: boolean;
		hint?: string;
		/** Shown in place of the hint, and reddens the control's boundary. */
		error?: string;
		disabled?: boolean;
		class?: string;
		/** `<option>`s for `as="select"`. */
		children?: Snippet;
		/** A control this component does not render itself. */
		control?: Snippet;
		// The control's own attributes — placeholder, min, step, required,
		// autocomplete, oninput — spread through `...rest`, so the props type
		// has to admit them or the promise in the header above is a lie.
	} & Omit<HTMLInputAttributes, 'type' | 'value' | 'checked' | 'size'>;

	let {
		label = undefined,
		as = 'input',
		type = 'text',
		value = $bindable(),
		checked = $bindable(false),
		group = $bindable(),
		inline = false,
		hint = undefined,
		error = undefined,
		disabled = false,
		class: extra = '',
		children,
		control,
		...rest
	}: Props = $props();

	const classes = $derived(
		['field', inline && 'inline', error && 'invalid', extra].filter(Boolean).join(' ')
	);

	// `rest` is typed as the INPUT attribute set, because that is the control
	// this component renders nine times in ten and it is what makes
	// `placeholder`/`min`/`step` legal at a call site. A <select> and a
	// <textarea> take the same bag of leftovers through a widened view of it.
	const controlRest = $derived(rest as Record<string, unknown>);
</script>

<label class={classes}>
	{#if label}<span class="label">{label}</span>{/if}

	{#if control}
		{@render control()}
	{:else if as === 'select'}
		<select class="control" bind:value {disabled} {...controlRest}>{@render children?.()}</select>
	{:else if as === 'textarea'}
		<textarea class="control" bind:value {disabled} {...controlRest}></textarea>
	{:else if type === 'checkbox'}
		<input class="check" type="checkbox" bind:checked {disabled} {...rest} />
	{:else if type === 'radio'}
		<input class="check" type="radio" bind:group {value} {disabled} {...rest} />
	{:else if type === 'number'}
		<input class="control" type="number" bind:value {disabled} {...rest} />
	{:else if type === 'date'}
		<input class="control" type="date" bind:value {disabled} {...rest} />
	{:else if type === 'file'}
		<input class="control" type="file" {disabled} {...rest} />
	{:else}
		<input class="control" type="text" bind:value {disabled} {...rest} />
	{/if}

	{#if error}
		<span class="error">{error}</span>
	{:else if hint}
		<span class="hint">{hint}</span>
	{/if}
</label>

<style>
	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		font-size: var(--text-sm);
		color: var(--color-text-subtle);
	}
	.inline {
		flex-direction: row;
		align-items: center;
		gap: var(--space-2);
	}
	/* In a checkbox row the caption reads as the control's own label, so it
	   follows the control and takes body weight rather than caption grey. */
	.inline .label {
		order: 1;
		color: var(--color-text);
	}

	.label {
		line-height: var(--leading-normal);
	}

	.control {
		padding: var(--space-2);
		background: var(--color-control-surface);
		border: 1px solid var(--color-control-border);
		border-radius: var(--radius-md);
		color: var(--color-control-text);
		font: inherit;
		font-size: var(--text-lg);
	}
	.control::placeholder {
		color: var(--color-control-placeholder);
	}
	.control:focus-visible {
		outline: none;
		border-color: var(--color-border-focus);
		box-shadow: var(--shadow-focus);
	}
	.control:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}
	textarea.control {
		resize: vertical;
	}

	.check {
		accent-color: var(--color-accent);
		width: var(--space-4);
		height: var(--space-4);
		margin: var(--space-0);
	}

	.invalid .control {
		border-color: var(--color-danger);
	}

	.hint {
		font-size: var(--text-xs);
		color: var(--color-text-subtle);
	}
	.error {
		font-size: var(--text-xs);
		color: var(--color-danger-text);
	}
</style>
