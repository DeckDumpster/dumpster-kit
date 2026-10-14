import { test } from 'node:test';
import assert from 'node:assert/strict';
import Segmented from '../../components/Segmented.svelte';
import { markup, styles, rootClass, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

const VIEWS = [
	{ value: 'grid', label: '▦', ariaLabel: 'Grid view', title: 'Grid', testid: 'view-grid' },
	{ value: 'table', label: '≡', ariaLabel: 'Table view', title: 'Table', testid: 'view-table' }
];

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(Segmented, { items: VIEWS, label: 'View', ...props });

test('Segmented renders one button per item inside a named group', () => {
	const html = body({ value: 'grid' });
	assert.match(html, /role="group"/);
	assert.match(html, /aria-label="View"/);
	assert.equal(html.match(/<button/g)?.length, 2);
	assert.match(html, /type="button"/, 'never submits a form by accident');
	assert.match(html, /▦/);
	assert.match(html, /≡/);
	// Glyph segments carry their own name and tooltip.
	assert.match(html, /aria-label="Grid view"/);
	assert.match(html, /title="Table"/);
	assert.match(html, /data-testid="view-grid"/);
});

test('Segmented marks the selected item and only that one', () => {
	const html = body({ value: 'table' });
	assert.equal(html.match(/aria-pressed="true"/g)?.length, 1);
	assert.match(html, /aria-pressed="true"[^>]*data-testid="view-table"|data-testid="view-table"[^>]*aria-pressed="true"/s);

	// A value matching nothing presses nothing — a segmented control is not
	// obliged to have an answer yet.
	assert.doesNotMatch(body({ value: 'chart' }), /aria-pressed="true"/);
});

test('Segmented respects its variants', () => {
	assert.match(rootClass(body()), /\bjoined\b/, 'variant defaults to joined');
	assert.match(rootClass(body({ variant: 'pill' })), /\bpill\b/);
	for (const size of ['sm', 'md', 'lg']) {
		assert.match(rootClass(body({ size })), new RegExp(`\\bs-${size}\\b`));
	}
	assert.match(rootClass(body()), /\bs-md\b/, 'size defaults to md');
	assert.match(rootClass(body({ equal: true })), /\bequal\b/);
	assert.doesNotMatch(rootClass(body()), /\bequal\b/);
});

test('Segmented renders an adornment beside the selected item', () => {
	const html = body({
		value: 'grid',
		adornment: (/** @type {any} */ r, /** @type {any} */ _item, /** @type {boolean} */ on) =>
			r.push(on ? '<span class="caret">▲</span>' : '')
	});
	assert.equal(html.match(/class="caret"/g)?.length, 1);
});

test('Segmented disables an item without disabling the group', () => {
	const html = body({ items: [VIEWS[0], { ...VIEWS[1], disabled: true }] });
	assert.equal(html.match(/disabled/g)?.length, 1);
});

test('Segmented emits no hardcoded colour', () => {
	assertTokenOnly('Segmented.svelte');
	const css = styles(Segmented, { items: VIEWS, label: 'View' });
	// The active segment is a wash and an edge, never a solid brand slab.
	assert.match(css, /\.pill[^{}]*\.segment\.on[^{}]*\{[^}]*var\(--color-surface-selected\)/);
	assert.doesNotMatch(css, /background:\s*var\(--color-accent\)/);
	assertTokensDeclared(css);
});
