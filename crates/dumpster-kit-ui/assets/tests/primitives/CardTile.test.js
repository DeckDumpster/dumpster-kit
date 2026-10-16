import { test } from 'node:test';
import assert from 'node:assert/strict';
import CardTile from '../../components/CardTile.svelte';
import { markup, styles, rootClass, assertTokenOnly, assertTokensDeclared } from '../support/render.js';

const PIPS = [
	{ id: 'a', label: 'Normal', color: 'var(--color-chart-unknown)', owned: 0 },
	{ id: 'b', label: 'Foil', color: 'var(--color-chart-unknown)', owned: 3 }
];

/** @param {Record<string, unknown>} [props] */
const body = (props = {}) => markup(CardTile, { name: 'Pikachu', number: '25', image: '/p.png', pips: PIPS, ...props });

/** The declarations of the first rule whose selector matches `selector`. */
function rule(scoped, selector) {
	const css = scoped
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/:where\(\.svelte-[a-z0-9]+\)/g, '')
		.replace(/\.svelte-[a-z0-9]+/g, '');
	const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	const m = new RegExp(`(?:^|[},\\s])${escaped}\\s*\\{([^}]*)\\}`).exec(css);
	assert.ok(m, `no rule for ${selector}`);
	return m[1];
}

test('CardTile renders image, number and one pip per printing in a single footer', () => {
	const html = body();
	assert.match(html, /<img[^>]*src="\/p\.png"/);
	assert.match(html, />#25</);
	assert.equal((html.match(/class="pip[ "]/g) ?? []).length, 2);
	assert.equal((html.match(/class="footer/g) ?? []).length, 1);
	assert.match(html, /class="pip[^"]* owned/, 'an owned printing is filled');
	assert.match(html, />3</, 'a count over one is shown');
});

test('there are no view modes: the footer is the same with or without a name label', () => {
	assert.doesNotMatch(body(), /class="name|class="meta/);
});

test('CardTile respects rarity, foil, rotate, missing and price', () => {
	assert.match(rootClass(body()), /\br-unranked\b/);
	assert.match(rootClass(body({ rarity: 'secret' })), /\br-secret\b/);
	assert.match(rootClass(body({ foil: true })), /\bfoil\b/);
	assert.match(rootClass(body({ rotate: true })), /\brotate\b/);
	assert.match(rootClass(body({ missing: true })), /\bmissing\b/);
	assert.doesNotMatch(rootClass(body()), /\b(foil|rotate|missing)\b/);
	assert.match(body({ price: '$1.25' }), /class="price[^"]*">\$1\.25</);
	assert.doesNotMatch(body(), /class="price/);
});

test('every rarity bucket resolves to its token', () => {
	const css = styles(CardTile, { name: 'x' });
	for (const r of ['common', 'uncommon', 'rare', 'promo', 'holo', 'ultra', 'special', 'secret', 'unranked']) {
		assert.match(rule(css, `.r-${r}`), new RegExp(`--tile-rarity:\\s*var\\(--color-rarity-${r}\\)`));
	}
	assert.match(rule(css, '.art'), /border:\s*2px solid var\(--tile-rarity\)/, 'the outline is the rarity colour');
});

test('the footer is one line: nowrap, clipped, pips shrink instead of wrapping', () => {
	const css = styles(CardTile, { name: 'x' });
	const footer = rule(css, '.footer');
	assert.match(footer, /flex-wrap:\s*nowrap/);
	assert.match(footer, /overflow:\s*hidden/);
	assert.match(rule(css, '.num'), /white-space:\s*nowrap/);
	assert.match(rule(css, '.num'), /text-overflow:\s*ellipsis/);
	assert.match(rule(css, '.pips'), /flex-wrap:\s*nowrap/);
	assert.match(rule(css, '.pip'), /aspect-ratio:\s*1\b/, 'pips stay round when they shrink');
	assert.doesNotMatch(css, /flex-wrap:\s*wrap/);
});

/**
 * Arithmetic model of that CSS: the footer's content box is the tile width;
 * the pips hold their 1.25rem basis, shrink together to fit, and the number
 * takes what is left (never negative, never pushing the pips).
 */
function footerFits(tile, pips, gap = 1, pip = 20, number = 40) {
	const pipsWidth = Math.min(pips * pip + Math.max(0, pips - 1) * 2, tile - gap);
	const numWidth = Math.max(0, Math.min(number, tile - pipsWidth - gap));
	return { total: pipsWidth + numWidth + gap, pipsWidth };
}

test('no overflow at 390 and 1280 for 2..8 columns', () => {
	for (const viewport of [390, 1280]) {
		for (let cols = 2; cols <= 8; cols++) {
			const tile = (viewport - 16 * 2 - 8 * (cols - 1)) / cols;
			for (const pips of [1, 2, 4, 6]) {
				const { total } = footerFits(tile, pips);
				assert.ok(total <= tile + 1e-9, `${viewport}px/${cols} cols/${pips} pips: ${total} > ${tile}`);
			}
		}
	}
});

test('pips sit at the same right edge and size whatever the number length', () => {
	const css = styles(CardTile, { name: 'x' });
	assert.match(rule(css, '.pips'), /justify-content:\s*flex-end/);
	assert.match(rule(css, '.footer'), /justify-content:\s*space-between/);
	assert.match(body({ number: '' }), /class="num[^"]*"><\/span>/, 'an empty number keeps the slot');
});

test('a landscape card is turned upright inside the portrait box', () => {
	const css = styles(CardTile, { name: 'x' });
	assert.match(rule(css, '.rotate .art img'), /rotate\(90deg\)/);
	assert.match(rule(css, '.art'), /aspect-ratio:\s*5\s*\/\s*7/);
});

test('CardTile emits no hardcoded colour', () => {
	assertTokenOnly('CardTile.svelte');
	assertTokensDeclared(styles(CardTile, { name: 'x' }));
});
