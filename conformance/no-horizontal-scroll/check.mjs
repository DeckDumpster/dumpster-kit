// law-no-horizontal-scroll-on-mobile: no route scrolls sideways at phone or tablet width.
// Driven by a consumer's UI tier with its own Playwright: `page` is any Playwright Page.

export const VIEWPORTS = [
	{ name: 'phone', width: 390, height: 844 },
	{ name: 'tablet', width: 768, height: 1024 },
];

// Runs in the page. Returns scrollWidth/innerWidth and the widest element reaching past the right edge.
function measure() {
	const root = document.scrollingElement || document.documentElement;
	const inner = window.innerWidth;
	let widest = null;
	for (const el of document.querySelectorAll('body *')) {
		const r = el.getBoundingClientRect();
		if (r.width === 0 || r.height === 0) continue;
		const style = getComputedStyle(el);
		if (style.position === 'fixed' || style.visibility === 'hidden') continue;
		// Content inside an element that scrolls on its own is not page overflow.
		let clipped = false;
		for (let p = el.parentElement; p && p !== document.body; p = p.parentElement) {
			const ox = getComputedStyle(p).overflowX;
			if (ox === 'auto' || ox === 'scroll' || ox === 'hidden' || ox === 'clip') {
				clipped = true;
				break;
			}
		}
		if (clipped) continue;
		const over = r.right - inner;
		if (over > 0.5 && (!widest || over > widest.over)) {
			const id = el.id ? `#${el.id}` : '';
			const cls = typeof el.className === 'string' && el.className.trim()
				? '.' + el.className.trim().split(/\s+/).join('.')
				: '';
			widest = { selector: `${el.tagName.toLowerCase()}${id}${cls}`, width: Math.round(r.width), over: Math.round(over) };
		}
	}
	return { scrollWidth: root.scrollWidth, innerWidth: inner, widest };
}

/** Measure one already-loaded page at its current viewport. */
export async function measureOverflow(page) {
	return page.evaluate(measure);
}

/**
 * Load every route at every viewport; return one failure per (route, viewport) that scrolls sideways.
 * `routes` is the site's route manifest: strings, or { path, setup?(page) } to reach an expanded state.
 */
export async function findHorizontalScroll({ page, baseUrl, routes, viewports = VIEWPORTS }) {
	if (!routes || routes.length === 0) throw new Error('no routes given: a check over nothing passes vacuously');
	const failures = [];
	for (const vp of viewports) {
		await page.setViewportSize({ width: vp.width, height: vp.height });
		for (const route of routes) {
			const path = typeof route === 'string' ? route : route.path;
			await page.goto(new URL(path, baseUrl).toString(), { waitUntil: 'networkidle' });
			const states = typeof route === 'string' || !route.states ? [{ name: 'default' }] : route.states;
			for (const state of states) {
				if (state.setup) await state.setup(page);
				const m = await measureOverflow(page);
				if (m.scrollWidth > m.innerWidth) {
					failures.push({ route: path, state: state.name, viewport: `${vp.width}x${vp.height}`, ...m });
				}
			}
		}
	}
	return failures;
}

export function describeFailure(f) {
	const w = f.widest ? `widest element ${f.widest.selector} (${f.widest.width}px wide, ${f.widest.over}px past the edge)` : 'no single element overflows (check margins/transforms)';
	return `${f.route} [${f.state}] at ${f.viewport}: scrollWidth ${f.scrollWidth} > innerWidth ${f.innerWidth}; ${w}`;
}

/** Throws, naming every route and its widest offender, when any route scrolls sideways. */
export async function assertNoHorizontalScroll(opts) {
	const failures = await findHorizontalScroll(opts);
	if (failures.length) throw new Error('horizontal scroll:\n' + failures.map(describeFailure).join('\n'));
}

/** Read a route manifest: JSON `{ "routes": ["/", "/browse", ...] }`. */
export async function loadRoutes(file) {
	const { readFile } = await import('node:fs/promises');
	const parsed = JSON.parse(await readFile(file, 'utf8'));
	if (!Array.isArray(parsed.routes) || parsed.routes.length === 0) throw new Error(`${file}: no routes`);
	return parsed.routes;
}
