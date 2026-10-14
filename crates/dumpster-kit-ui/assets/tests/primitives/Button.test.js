import { test } from 'node:test';
import assert from 'node:assert/strict';
import Button from '../../components/Button.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(Button, { children: slot('Save'), ...props });

test('Button renders a button with its label', () => {
	const html = body();
	assert.match(html, /Save/);
	assert.match(html, /<button/);
	assert.match(html, /type="button"/, 'defaults to type=button so it never submits a form by accident');
	assert.match(rootClass(html), /\bprimary\b/);
});

test('Button respects its variants', () => {
	for (const variant of ['primary', 'secondary', 'ghost', 'danger', 'link']) {
		assert.match(
			rootClass(body({ variant })),
			new RegExp(`\\b${variant}\\b`),
			`variant "${variant}" must reach the markup`
		);
	}
	assert.match(rootClass(body({ size: 'sm' })), /\bs-sm\b/);
	assert.match(rootClass(body({ size: 'lg' })), /\bs-lg\b/);
	assert.match(rootClass(body()), /\bs-md\b/, 'size defaults to md');
	assert.match(rootClass(body({ block: true })), /\bblock\b/);
});

test('Button renders an anchor when given an href', () => {
	const html = body({ href: '/ingest/order', variant: 'primary' });
	assert.match(html, /<a /);
	assert.match(html, /href="\/ingest\/order"/);
	assert.doesNotMatch(html, /<button/);
});

test('Button disables both of its forms', () => {
	assert.match(body({ disabled: true }), /disabled/);

	// HTML has no disabled anchor, so the link form has to say so itself —
	// and must not stay reachable by keyboard or click.
	const link = body({ href: '/orders', disabled: true });
	assert.match(link, /aria-disabled="true"/);
	assert.match(link, /tabindex="-1"/);
	assert.doesNotMatch(link, /href="\/orders"/);
});

test('Button forwards arbitrary attributes to the element', () => {
	assert.match(body({ title: 'Save this copy' }), /title="Save this copy"/);
	assert.match(body({ type: 'submit' }), /type="submit"/);
});

test('Button emits no hardcoded colour', () => {
	assertTokenOnly('Button.svelte');
	const css = styles(Button, { children: slot('Save') });
	// The brand fill is unchanged; what the token layer moved is the LABEL.
	assert.match(css, /background:\s*var\(--color-accent\)/);
	assert.match(css, /color:\s*var\(--color-on-accent\)/);
	assertTokensDeclared(css);
});
