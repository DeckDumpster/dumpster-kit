/**
 * The UI primitives — The kit's visual vocabulary.
 *
 * Routes import from here and stop making their own decisions about surfaces,
 * fills, rules and spacing:
 *
 *     import { Panel, Button, Field } from '@dumpster-kit/ui';
 *
 * Every one of them is styled entirely from SEMANTIC tokens
 * (`--color-*`, `--space-*`, `--text-*`, `--radius-*`, `--shadow-*`). None
 * reaches into the `--pd-*` reference layer, and none contains a colour
 * literal — `assets/tests/primitives/` fails per primitive if that stops
 * being true.
 *
 * A route that needs a variant this file doesn't have should ADD the variant
 * here rather than restyle a primitive at the call site: the moment two
 * routes patch the same primitive differently, the system is back to taste.
 */

export { default as Badge } from './Badge.svelte';
export { default as Button } from './Button.svelte';
export { default as EmptyState } from './EmptyState.svelte';
export { default as Field } from './Field.svelte';
export { default as Menu } from './Menu.svelte';
export { default as Panel } from './Panel.svelte';
export { default as ProgressBar } from './ProgressBar.svelte';
export { default as SearchField } from './SearchField.svelte';
export { default as SectionHeader } from './SectionHeader.svelte';
export { default as Segmented } from './Segmented.svelte';
export { default as Toolbar } from './Toolbar.svelte';
