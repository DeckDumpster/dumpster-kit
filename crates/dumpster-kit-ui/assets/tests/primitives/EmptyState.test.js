import { test } from 'node:test';
import assert from 'node:assert/strict';
import EmptyState from '../../components/EmptyState.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(EmptyState, { title: 'No decks yet.', ...props });

test('EmptyState renders its message', () => {
	const html = body();
	assert.match(html, /No decks yet\./);
	assert.match(rootClass(html), /\bempty\b/);
});

test('EmptyState respects its variants', () => {
	assert.match(rootClass(body({ tone: 'success' })), /\bt-success\b/);
	assert.match(rootClass(body()), /\bt-neutral\b/);
	assert.match(rootClass(body({ size: 'sm' })), /\bs-sm\b/);
	assert.match(rootClass(body()), /\bs-md\b/);
});

test('EmptyState draws the description and action only when given them', () => {
	const bare = body();
	assert.doesNotMatch(bare, /class="description/);
	assert.doesNotMatch(bare, /class="action/);

	const full = body({
		description: 'Create one to group cards for play.',
		action: slot('<button>New deck</button>')
	});
	assert.match(full, /class="description/);
	assert.match(full, /Create one to group cards for play\./);
	assert.match(full, /<button>New deck<\/button>/);
});

test('the page-level size draws a surface, the inline one does not', () => {
	// pd-0ksp: a route whose list is empty used to render a heading, a bare
	// control and an ocean of page fill. `md` is the frame that replaces it —
	// if it stops drawing one, every empty route silently goes back to prose.
	const css = styles(EmptyState, { title: 'x' });
	const md = css.match(/\.s-md[^{]*\{([^}]*)\}/)?.[1] ?? '';
	const sm = css.match(/\.s-sm[^{]*\{([^}]*)\}/)?.[1] ?? '';
	assert.match(md, /border:[^;]*dashed/);
	assert.match(md, /background:\s*var\(--color-surface-well\)/);
	// `sm` sits inside a box that already exists (a panel, a picker list, a
	// chart slot); a second frame there is noise.
	assert.doesNotMatch(sm, /border|background/);
});

test('EmptyState emits no hardcoded colour', () => {
	assertTokenOnly('EmptyState.svelte');
	const css = styles(EmptyState, { title: 'x' });
	// Empty is sometimes the GOOD outcome (/ingest/unresolved's cleared queue),
	// and that case has to be able to say so.
	assert.match(css, /\.t-success[^{]*\{[^}]*var\(--color-success-text\)/);
	assertTokensDeclared(css);
});
