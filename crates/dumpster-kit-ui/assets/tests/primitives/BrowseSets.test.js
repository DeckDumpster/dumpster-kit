import { test } from 'node:test';
import assert from 'node:assert/strict';
import JumpRow from '../../components/JumpRow.svelte';
import SetSection from '../../components/SetSection.svelte';
import TileGrid from '../../components/TileGrid.svelte';
import { markup, styles, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

// The browse-sets page must never scroll sideways on a phone. No browser runs
// in this suite, so "no overflow at 390 and 1280" is asserted as the CSS
// contract that guarantees it, per viewport: every row wraps, every track can
// shrink below 390px, every text node may break.
const VIEWPORTS = [390, 1280];

/** The declaration block of the first rule whose selector matches. */
const rule = (css, selector) => {
	const m = new RegExp(`(?:^|\\})\\s*[^{}]*${selector}[^{}]*\\{([^}]*)\\}`).exec(css);
	assert.ok(m, `no rule for ${selector}`);
	return m[1];
};

test('JumpRow renders a chip per item and wraps', () => {
	const props = { items: [{ key: 'a', label: 'Scarlet & Violet', count: 12, active: true }, { key: 'b', label: 'Bundles' }] };
	const html = markup(JumpRow, props);
	assert.match(html, /Scarlet &amp; Violet/);
	assert.match(html, /<button[^>]*class="[^"]*chip[^"]*active/);
	const css = styles(JumpRow, props);
	for (const w of VIEWPORTS) {
		assert.match(rule(css, '\\.jump'), /flex-wrap:\s*wrap/, `chip row wraps at ${w}`);
		assert.match(rule(css, '\\.chip'), /max-width:\s*100%/, `chip fits the row at ${w}`);
		assert.match(rule(css, '\\.name'), /overflow-wrap:\s*anywhere/, `chip label breaks at ${w}`);
	}
	assertTokenOnly('JumpRow.svelte');
	assertTokensDeclared(css);
});

test('SetSection header wraps its title and counts, and collapses', () => {
	const props = { title: 'Mega Evolution', meta: '12 sets · 1,204 / 3,000 cards (40%)', children: slot('<p>body</p>') };
	const open = markup(SetSection, props);
	assert.match(open, /aria-expanded="true"/);
	assert.match(open, /<p>body<\/p>/);
	const closed = markup(SetSection, { ...props, open: false });
	assert.match(closed, /aria-expanded="false"/);
	assert.doesNotMatch(closed, /<p>body<\/p>/);
	const css = styles(SetSection, props);
	for (const w of VIEWPORTS) {
		assert.match(rule(css, '\\.head'), /flex-wrap:\s*wrap/, `header wraps at ${w}`);
		assert.match(rule(css, '\\.title'), /overflow-wrap:\s*anywhere/, `title breaks at ${w}`);
		assert.match(rule(css, '\\.meta'), /overflow-wrap:\s*anywhere/, `counts break at ${w}`);
	}
	assertTokenOnly('SetSection.svelte');
	assertTokensDeclared(css);
});

test('TileGrid columns can shrink below the phone viewport', () => {
	const props = { children: slot('<a>tile</a>') };
	assert.match(markup(TileGrid, props), /<a>tile<\/a>/);
	const css = styles(TileGrid, props);
	const grid = rule(css, '\\.grid');
	assert.match(grid, /minmax\(min\(100%,\s*14rem\),\s*1fr\)/, 'a track never exceeds its container');
	assert.match(css, /\.grid[^{>]*>\s*\*\s*\{[^}]*min-width:\s*0/, 'tiles may shrink so names wrap');
	assertTokenOnly('TileGrid.svelte');
	assertTokensDeclared(css);
});
