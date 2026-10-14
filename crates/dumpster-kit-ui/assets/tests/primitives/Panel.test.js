import { test } from 'node:test';
import assert from 'node:assert/strict';
import Panel from '../../components/Panel.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(Panel, { children: slot(), ...props });

test('Panel renders its children inside a surface', () => {
	const html = markup(Panel, { children: slot('stats') });
	assert.match(html, /stats/);
	assert.match(rootClass(html), /\bpanel\b/);
	assert.match(html, /<div class="panel /);
});

test('Panel respects its variants', () => {
	assert.match(rootClass(body({ variant: 'raised' })), /\bv-raised\b/);
	assert.match(rootClass(body({ variant: 'sunken' })), /\bv-sunken\b/);
	assert.match(rootClass(body({ variant: 'overlay' })), /\bv-overlay\b/);

	assert.match(rootClass(body({ padding: 'none' })), /\bp-none\b/);
	assert.match(rootClass(body({ padding: 'lg' })), /\bp-lg\b/);
	assert.match(rootClass(body()), /\bp-md\b/, 'padding defaults to md');

	assert.match(rootClass(body({ elevation: 'md' })), /\be-md\b/);
	assert.match(rootClass(body({ interactive: true })), /\binteractive\b/);
});

test('Panel with an href renders a link that answers to hover', () => {
	const html = body({ href: '/browse/base1' });
	assert.match(html, /<a href="\/browse\/base1"/);
	// A clickable panel gets the hover affordance whether or not the caller
	// asked for it — that is the tile in /browse, /binders and /decks.
	assert.match(rootClass(html), /\binteractive\b/);
});

test('Panel keeps caller classes alongside its own', () => {
	assert.match(rootClass(body({ class: 'nav' })), /\bpanel\b.*\bnav\b/);
});

test('Panel emits no hardcoded colour', () => {
	assertTokenOnly('Panel.svelte');
	const css = styles(Panel, { children: slot() });
	assert.match(css, /var\(--color-surface-panel\)/);
	assertTokensDeclared(css);
});
