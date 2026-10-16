// Proves the check fails on a 2000px row (naming it) and passes on a wrapping page.
// Needs playwright resolvable; consumers' UI tiers have it, the kit's own node tests do not, so skip without it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { findHorizontalScroll, describeFailure, assertNoHorizontalScroll } from './check.mjs';

let chromium;
try {
	({ chromium } = createRequire(import.meta.url)('playwright'));
} catch {}

test('no-horizontal-scroll check', { skip: !chromium && 'playwright not installed' }, async () => {
	const dir = fileURLToPath(new URL('./fixtures/', import.meta.url));
	const server = http.createServer(async (req, res) => {
		try {
			res.setHeader('content-type', 'text/html');
			res.end(await readFile(dir + req.url));
		} catch {
			res.statusCode = 404;
			res.end();
		}
	}).listen(0);
	const baseUrl = `http://127.0.0.1:${server.address().port}`;
	let browser;
	try {
		browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || undefined });
		const page = await browser.newPage();
		const failures = await findHorizontalScroll({ page, baseUrl, routes: ['/wide.html'] });
		assert.equal(failures.length, 2, 'both viewports fail');
		assert.match(describeFailure(failures[0]), /\/wide\.html.*div\.chips.*2000px wide/);
		await assert.rejects(assertNoHorizontalScroll({ page, baseUrl, routes: ['/ok.html', '/wide.html'] }), /wide\.html/);
		await assertNoHorizontalScroll({ page, baseUrl, routes: ['/ok.html'] });
	} finally {
		await browser?.close();
		server.closeAllConnections();
		server.close();
	}
});
