/**
 * The shared half of every primitive's render test.
 *
 * Three assertions recur per primitive, so they live here once:
 *   markup()      — server-render a component to HTML for the "renders" and
 *                   "respects variants" cases.
 *   styles()      — the component's own scoped CSS, for the colour audit.
 *   assertTokenOnly() — the standing contract: a primitive may name visual
 *                   values only through SEMANTIC tokens. No raw literal, and
 *                   no reach into the `--pd-*` reference layer (which would
 *                   collapse the two-layer split tokens.css exists to keep).
 *
 * `tokens.contrast.test.js` already enforces both rules across all of
 * frontend/src. Repeating them per primitive is deliberate: these components
 * are the vocabulary 20 routes are about to migrate onto, and a failure named
 * "Button emits a hardcoded colour" is worth more than one naming the tree.
 */

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { render } from 'svelte/server';

const here = dirname(fileURLToPath(import.meta.url));

/** @type {string} */
export const uiDir = join(here, '../../components');

/**
 * Server-render a component and return its markup.
 * @param {any} Component
 * @param {Record<string, unknown>} [props]
 * @returns {string}
 */
export function markup(Component, props = {}) {
	return render(Component, { props }).body;
}

/**
 * A component's own scoped stylesheet, as compiled into the render payload.
 * @param {any} Component
 * @param {Record<string, unknown>} [props]
 * @returns {string}
 */
export function styles(Component, props = {}) {
	const head = render(Component, { props }).head;
	return [...head.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)].map((m) => m[1]).join('\n');
}

/**
 * A snippet that renders literal markup — the stand-in for whatever a caller
 * would put in `children`, `actions` or `control`.
 * @param {string} [html]
 * @returns {any}
 */
export function slot(html = '') {
	return (/** @type {{ push: (s: string) => void }} */ renderer) => renderer.push(html);
}

/**
 * The `class="…"` value of the first element in a rendered fragment.
 * @param {string} html
 * @returns {string}
 */
export function rootClass(html) {
	const m = /class="([^"]*)"/.exec(html);
	return m ? m[1] : '';
}

/**
 * A primitive may reference visual values only through the semantic layer.
 * Checked against the source file rather than the rendered CSS so that a
 * literal smuggled into an inline `style` attribute is caught too.
 * @param {string} file basename inside src/lib/components/ui
 */
export function assertTokenOnly(file) {
	// Comments are stripped first, the same way tokens.contrast.test.js does it:
	// these components document WHICH literal each token replaced, and that
	// prose is the opposite of a violation.
	const source = readFileSync(join(uiDir, file), 'utf8')
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/<!--[\s\S]*?-->/g, '')
		.split('\n')
		.filter((line) => !/^\s*(\/\/|\*)/.test(line))
		.join('\n');

	const hex = [...source.matchAll(/(?<![\w&])#[0-9a-fA-F]{3,8}(?![\w-])/g)].map((m) => m[0]);
	assert.deepEqual(hex, [], `${file} contains raw colour literals: ${hex.join(', ')}`);

	const fn = [...source.matchAll(/\b(rgba?|hsla?|color-mix)\(/g)].map((m) => m[0]);
	assert.deepEqual(fn, [], `${file} builds colours in CSS instead of naming a token: ${fn.join(', ')}`);

	const reference = [...source.matchAll(/var\(\s*--pd-[a-z0-9-]+/g)].map((m) => m[0]);
	assert.deepEqual(
		reference,
		[],
		`${file} reaches into the reference layer; components use semantic --color-* roles: ${reference.join(', ')}`
	);
}

/**
 * Every custom property the component's CSS reads, deduplicated.
 * @param {string} css
 * @returns {string[]}
 */
export function tokensUsed(css) {
	return [...new Set([...css.matchAll(/var\(\s*(--[a-z0-9-]+)/g)].map((m) => m[1]))].sort();
}

/**
 * Assert that the component's CSS resolves only tokens that tokens.css declares.
 * A typo'd token name is invisible in a browser (it just falls back to nothing);
 * here it fails.
 * @param {string} css
 */
export function assertTokensDeclared(css) {
	const tokensCss = readFileSync(join(here, '../../tokens.css'), 'utf8').replace(
		/\/\*[\s\S]*?\*\//g,
		''
	);
	const declared = new Set([...tokensCss.matchAll(/(--[a-z0-9-]+)\s*:/g)].map((m) => m[1]));
	const unknown = tokensUsed(css).filter(
		// Component-local custom properties (`--badge-fill` and friends) are set
		// and read inside the component itself; only global roles must exist.
		(t) => !declared.has(t) && /^--(color|space|text|font|weight|leading|radius|shadow|dur|ease|gradient)-/.test(t)
	);
	assert.deepEqual(unknown, [], `these tokens are not declared in tokens.css: ${unknown.join(', ')}`);
}
