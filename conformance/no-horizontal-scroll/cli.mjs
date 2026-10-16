#!/usr/bin/env node
// node cli.mjs --base-url http://localhost:8080 --routes routes.json
// Needs `playwright` (or @playwright/test) resolvable from the caller's cwd.
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { assertNoHorizontalScroll, loadRoutes } from './check.mjs';

const args = Object.fromEntries(process.argv.slice(2).reduce((a, v, i, all) => (v.startsWith('--') ? [...a, [v.slice(2), all[i + 1]]] : a), []));
if (!args['base-url'] || !args.routes) {
	console.error('usage: cli.mjs --base-url <url> --routes <routes.json>');
	process.exit(2);
}
const require = createRequire(resolve('package.json'));
let chromium;
try {
	({ chromium } = require('playwright'));
} catch {
	({ chromium } = require('@playwright/test'));
}
const browser = await chromium.launch();
try {
	const page = await browser.newPage();
	await assertNoHorizontalScroll({ page, baseUrl: args['base-url'], routes: await loadRoutes(args.routes) });
	console.log('no horizontal scroll on any route');
} catch (e) {
	console.error(e.message);
	process.exitCode = 1;
} finally {
	await browser.close();
}
