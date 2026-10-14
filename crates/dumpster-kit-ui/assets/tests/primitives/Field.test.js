import { test } from 'node:test';
import assert from 'node:assert/strict';
import Field from '../../components/Field.svelte';
import { markup, styles, rootClass, slot, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(Field, props);

test('Field renders a label wrapping its control', () => {
	const html = body({ label: 'Binder name' });
	assert.match(html, /<label/);
	assert.match(html, /Binder name/);
	assert.match(html, /<input[^>]*type="text"/);
	assert.match(rootClass(html), /\bfield\b/);
});

test('Field respects its control variants', () => {
	assert.match(body({ as: 'select' }), /<select/);
	assert.match(body({ as: 'textarea' }), /<textarea/);
	assert.match(body({ type: 'number' }), /type="number"/);
	assert.match(body({ type: 'date' }), /type="date"/);
	assert.match(body({ type: 'checkbox' }), /type="checkbox"/);
	assert.match(body({ type: 'radio' }), /type="radio"/);
	assert.match(body({ type: 'file' }), /type="file"/);
	assert.match(rootClass(body({ inline: true })), /\binline\b/);
});

test('Field renders select options from its children', () => {
	const html = body({ as: 'select', children: slot('<option>NM</option>') });
	assert.match(html, /<select/);
	assert.match(html, /<option>NM<\/option>/);
});

test('Field takes a control it does not render itself', () => {
	const html = body({ label: 'Card', control: slot('<div id="picker"></div>') });
	assert.match(html, /id="picker"/);
	assert.doesNotMatch(html, /<input/, 'the snippet replaces the built-in control entirely');
	assert.match(html, /Card/, 'and still gets the caption');
});

test('Field shows a hint, and an error in its place', () => {
	assert.match(body({ hint: 'Optional' }), /Optional/);

	const invalid = body({ hint: 'Optional', error: 'Required' });
	assert.match(invalid, /Required/);
	assert.doesNotMatch(invalid, /Optional/, 'an error supersedes the hint rather than stacking with it');
	assert.match(rootClass(invalid), /\binvalid\b/);
});

test('Field forwards attributes to the control', () => {
	assert.match(body({ placeholder: 'e.g. Base Set' }), /placeholder="e.g. Base Set"/);
	assert.match(body({ disabled: true }), /disabled/);
});

test('Field emits no hardcoded colour', () => {
	assertTokenOnly('Field.svelte');
	const css = styles(Field, {});
	// WCAG 1.4.11: the control boundary is its own role, not the panel rule.
	assert.match(css, /border:\s*1px solid var\(--color-control-border\)/);
	assert.doesNotMatch(css, /border:\s*1px solid var\(--color-border\)/);
	assertTokensDeclared(css);
});
