import { test } from 'node:test';
import assert from 'node:assert/strict';
import Badge from '../../components/Badge.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(Badge, { children: slot('sealed'), ...props });

const TONES = ['neutral', 'accent', 'success', 'warning', 'danger', 'info'];

test('Badge renders its label in a chip', () => {
	const html = body();
	assert.match(html, /sealed/);
	assert.match(html, /<span/);
	assert.match(rootClass(html), /\bbadge\b/);
	assert.match(rootClass(html), /\bt-neutral\b.*\bsoft\b/, 'defaults to the un-coloured soft chip');
});

test('Badge respects its variants', () => {
	for (const tone of TONES) {
		assert.match(rootClass(body({ tone })), new RegExp(`\\bt-${tone}\\b`));
	}
	for (const variant of ['solid', 'soft', 'outline']) {
		assert.match(rootClass(body({ variant })), new RegExp(`\\b${variant}\\b`));
	}
	assert.match(rootClass(body({ shape: 'tag' })), /\btag\b/);
	assert.match(rootClass(body({ shape: 'pill' })), /\bpill\b/);
	assert.match(rootClass(body({ size: 'sm' })), /\bs-sm\b/);
});

test('every tone resolves all four of the roles the weights read', () => {
	// The weight rules (.solid/.soft/.outline) are written once against
	// component-local properties; a tone that forgets one silently paints
	// nothing. Cheaper to catch here than in a screenshot.
	const css = styles(Badge, { children: slot() });
	for (const tone of TONES) {
		const block = new RegExp(`\\.t-${tone}[^{]*\\{([^}]*)\\}`).exec(css);
		assert.ok(block, `tone ${tone} has no rule`);
		for (const role of ['fill', 'on-fill', 'surface', 'text', 'border']) {
			assert.match(block[1], new RegExp(`--badge-${role}:`), `tone ${tone} does not set --badge-${role}`);
		}
	}
});

test('Badge emits no hardcoded colour', () => {
	assertTokenOnly('Badge.svelte');
	const css = styles(Badge, { children: slot() });
	assertTokensDeclared(css);
});
