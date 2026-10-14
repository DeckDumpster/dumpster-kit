import { test } from 'node:test';
import assert from 'node:assert/strict';
import SectionHeader from '../../components/SectionHeader.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(SectionHeader, { title: 'Unresolved', ...props });

test('SectionHeader renders a heading', () => {
	const html = body();
	assert.match(html, /Unresolved/);
	assert.match(html, /<h2/, 'defaults to h2 — a section inside a page title');
	assert.match(rootClass(html), /\bhead\b/);
});

test('SectionHeader respects its variants', () => {
	assert.match(body({ level: 3 }), /<h3/);
	assert.match(body({ level: 1 }), /<h1/);
	for (const size of ['sm', 'md', 'lg']) {
		assert.match(rootClass(body({ size })), new RegExp(`\\bs-${size}\\b`));
	}
	for (const tone of ['subtle', 'accent', 'warning']) {
		assert.match(rootClass(body({ tone })), new RegExp(`\\bt-${tone}\\b`));
	}
	assert.match(rootClass(body({ divider: true })), /\bdivider\b/);
});

test('SectionHeader carries a count and controls on the heading row', () => {
	const html = body({ meta: '128 rows', actions: slot('<button>Clear</button>') });
	assert.match(html, /128 rows/);
	assert.match(html, /<button>Clear<\/button>/);
});

test('SectionHeader prefers a children snippet over the title prop', () => {
	const html = body({ children: slot('<a href="/browse/base1">Base Set</a>') });
	assert.match(html, /<a href="\/browse\/base1">Base Set<\/a>/);
	assert.doesNotMatch(html, /Unresolved/);
});

test('SectionHeader emits no hardcoded colour', () => {
	assertTokenOnly('SectionHeader.svelte');
	const css = styles(SectionHeader, { title: 'x' });
	assert.match(css, /color:\s*var\(--color-text-subtle\)/);
	assertTokensDeclared(css);
});
