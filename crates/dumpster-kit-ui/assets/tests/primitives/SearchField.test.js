import { test } from 'node:test';
import assert from 'node:assert/strict';
import SearchField from '../../components/SearchField.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(SearchField, props);

test('SearchField renders a text input in a positioned wrapper', () => {
	const html = body({ placeholder: 'Search this set…', label: 'Search this set' });
	assert.match(html, /<input/);
	assert.match(html, /type="text"/);
	assert.match(html, /placeholder="Search this set…"/);
	assert.match(html, /aria-label="Search this set"/);
	// The routes all set it, and a browser's own history dropdown over a live
	// filter is noise.
	assert.match(html, /autocomplete="off"/);
	assert.match(rootClass(html), /\bsearchfield\b/);
});

test('SearchField respects its variants', () => {
	for (const width of ['compact', 'comfortable', 'full']) {
		assert.match(rootClass(body({ width })), new RegExp(`\\bw-${width}\\b`));
	}
	assert.match(rootClass(body()), /\bw-comfortable\b/, 'width defaults to comfortable');
	assert.match(body({ invalid: true }), /\binvalid\b/);
	assert.doesNotMatch(body(), /\binvalid\b/);
	assert.match(body({ disabled: true }), /disabled/);
});

test('SearchField shows the clear button only when there is something to clear', () => {
	assert.doesNotMatch(body(), /aria-label="Clear search"/);
	assert.match(body({ value: 'char' }), /aria-label="Clear search"/);
	assert.doesNotMatch(
		body({ value: 'char', clearable: false }),
		/aria-label="Clear search"/,
		'a route that clears through its own control turns this off'
	);
});

test('SearchField forwards its own attributes to the input, not the wrapper', () => {
	// The UI intents drive the box by test id, and the combobox roles
	// /collection layers on top have to land on the control itself.
	const html = body({ 'data-testid': 'search-input', role: 'combobox' });
	assert.match(html, /<input[^>]*data-testid="search-input"/);
	assert.match(html, /<input[^>]*role="combobox"/);
	// The class is the wrapper's — it is the flex slot and the popover anchor.
	assert.match(rootClass(body({ class: 'mine' })), /\bmine\b/);
});

test('SearchField anchors a popover inside its wrapper', () => {
	const html = body({ value: 'x', children: slot('<ul id="search-ac"></ul>') });
	assert.match(html, /<ul id="search-ac">/);
});

test('SearchField emits no hardcoded colour', () => {
	assertTokenOnly('SearchField.svelte');
	const css = styles(SearchField);
	// An unparseable query reddens on the danger ramp, not the brand one.
	assert.match(css, /\.invalid[^{]*\{[^}]*var\(--color-danger\)/);
	assert.match(css, /background:\s*var\(--color-control-surface\)/);
	assertTokensDeclared(css);
});
