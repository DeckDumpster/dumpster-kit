import { test } from 'node:test';
import assert from 'node:assert/strict';
import Menu from '../../components/Menu.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

const ITEMS = [
	{ label: 'Export JSON (full backup)', href: '/api/export/json', download: true },
	{ label: 'Select', onclick: () => {}, testid: 'select-mode' }
];

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(Menu, { items: ITEMS, ...props });

test('Menu renders a closed burger trigger', () => {
	const html = body();
	assert.match(html, /⋯/);
	assert.match(html, /aria-label="Menu"/);
	assert.match(html, /aria-haspopup="true"/);
	assert.match(html, /aria-expanded="false"/);
	assert.match(rootClass(html), /\bmenuwrap\b/);
	// Nothing of the popover exists until it is open — including the backdrop.
	assert.doesNotMatch(html, /role="menu"/);
	assert.doesNotMatch(html, /Export JSON/);
	assert.doesNotMatch(html, /\bbackdrop\b/);
});

test('Menu renders its rows and its backdrop when open', () => {
	const html = body({ open: true });
	assert.match(html, /aria-expanded="true"/);
	assert.match(html, /role="menu"/);
	assert.equal(html.match(/role="menuitem"/g)?.length, 2);
	assert.match(html, /\bbackdrop\b/);

	// A row with an href is a link — that is what every export row is — and a
	// row without one is a button.
	assert.match(html, /<a[^>]*href="\/api\/export\/json"[^>]*download/);
	assert.match(html, /<button[^>]*data-testid="select-mode"/);
});

test('Menu respects its variants', () => {
	assert.match(body({ open: true }), /\ba-end\b/, 'hangs off the trigger’s right edge by default');
	assert.match(body({ open: true, align: 'start' }), /\ba-start\b/);
	assert.match(body({ open: true }), /\bw-md\b/);
	assert.match(body({ open: true, width: 'sm' }), /\bw-sm\b/);
	assert.match(body({ open: true, items: [{ label: 'Delete', tone: 'danger' }] }), /\bt-danger\b/);
	assert.match(body({ label: 'Collection actions' }), /aria-label="Collection actions"/);
});

test('Menu takes a custom trigger and extra rows', () => {
	assert.match(body({ trigger: slot('<span>More</span>') }), /<span>More<\/span>/);
	assert.doesNotMatch(body({ trigger: slot('<span>More</span>') }), /⋯/);

	const html = body({ open: true, items: [], children: slot('<hr />') });
	assert.match(html, /<hr/);
	assert.doesNotMatch(html, /role="menuitem"/);
});

test('Menu opens on the shared overlay surface, not one of its own', () => {
	// Panel `overlay` paints the popover; this component only places it. If it
	// ever grows a background of its own, the popover fill has two owners.
	assert.match(body({ open: true }), /class="[^"]*\bpanel\b[^"]*\bv-overlay\b/);
	const css = styles(Menu, { items: ITEMS, open: true });
	assert.match(css, /\.pop[^{]*\{[^}]*position:\s*absolute/);
	assert.doesNotMatch(css, /\.pop[^{]*\{[^}]*background/);
});

test('Menu emits no hardcoded colour', () => {
	assertTokenOnly('Menu.svelte');
	const css = styles(Menu, { items: ITEMS, open: true });
	assert.match(css, /\.item[^{}]*:hover[^{}]*\{[^}]*var\(--color-surface-hover\)/);
	// A destructive row reads on the danger ramp, never the brand one.
	assert.match(css, /\.t-danger[^{]*\{[^}]*var\(--color-danger-text\)/);
	assertTokensDeclared(css);
});
