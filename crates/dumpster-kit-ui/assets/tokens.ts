/**
 * Reading the token layer from script.
 *
 * CSS custom properties do not reach a `<canvas>`: Chart.js paints with
 * concrete colour strings, so the value has to be resolved before it is handed
 * over. This module is the only sanctioned way to do that — it reads the roles
 * off the live document rather than letting a component keep its own copy of a
 * hex, so `tokens.css` stays the single source of truth even for the charts.
 *
 * Everything that renders through CSS should use `var(--color-*)` directly and
 * never come here.
 */

/**
 * The computed value of a semantic token, e.g. `token('--color-chart-1')`.
 *
 * Returns `''` when there is no document (SSR / prerender). Every caller is a
 * chart that only builds in the browser, and Chart.js treats an empty colour as
 * "use the default" rather than throwing.
 */
export function token(name: string): string {
	if (typeof document === 'undefined') return '';
	return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/**
 * The eight categorical chart series roles, in order. A chart with more series
 * than this cycles — the palette is deliberately finite, because a ninth
 * distinguishable hue on a dark ground does not exist.
 */
export function chartPalette(): string[] {
	return [
		token('--color-chart-1'),
		token('--color-chart-2'),
		token('--color-chart-3'),
		token('--color-chart-4'),
		token('--color-chart-5'),
		token('--color-chart-6'),
		token('--color-chart-7'),
		token('--color-chart-8')
	];
}

/**
 * The translucent area fill under a series line, given that series' colour.
 *
 * The chart roles all resolve to 6-digit hex, so an 8-digit hex is the cheapest
 * correct way to add alpha; anything else falls back to no fill rather than
 * handing Chart.js a string it cannot parse.
 */
export function chartFill(color: string, alpha = '33'): string {
	return /^#[0-9a-fA-F]{6}$/.test(color) ? color + alpha : 'transparent';
}
