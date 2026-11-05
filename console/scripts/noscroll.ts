// Visits every console page at three sizes in light and dark and checks the page itself never scrolls
// (document.scrollingElement fits the viewport), plus 0 console errors. On a phone it also clicks each tab.
//   MOCK_PORT=7945 MOCK_AUTH=required bun scripts/mock-api.ts &  CK_DASH=7945 bun run dev -- --port 5180 &
//   bun scripts/noscroll.ts            (BASE, PW, SHOTS=<dir> optional)
import { mkdirSync } from 'node:fs';
const { chromium } = (await import(process.env.PW ?? `${process.env.HOME}/.atlas-shot/node_modules/playwright`)) as typeof import('playwright');

const BASE = process.env.BASE ?? 'http://localhost:5180';
const SHOTS = process.env.SHOTS;
const PAGES = ['/', '/nodes', '/map', '/live', '/sim', '/jobs', '/jobs/8', '/jobs/1', '/network', '/experiments', '/experiments/1', '/compare?jobs=1,4', '/alerts', '/report', '/jobs/new', '/events', '/events?view=logs', '/policies', '/settings', '/users', '/wall'];
const SIZES = [[1440, 900], [1024, 768], [390, 844]] as const;
const shotNames: Record<string, string> = { '/': 'overview', '/nodes': 'nodes', '/jobs': 'jobs', '/events': 'events', '/events?view=logs': 'logs', '/settings': 'system', '/experiments': 'experiments', '/experiments/1': 'experiment', '/compare?jobs=1,4': 'compare', '/alerts': 'alerts', '/report': 'report', '/jobs/8': 'job', '/jobs/1': 'jobfiles', '/network': 'network' };

// the cached browser may be a newer build than this playwright expects: CHROME=<path> overrides
const cache = `${process.env.HOME}/Library/Caches/ms-playwright`;
const browser = await chromium.launch({ executablePath: process.env.CHROME ?? (await Array.fromAsync(new Bun.Glob('chromium_headless_shell-*/chrome-headless-shell-mac-arm64/chrome-headless-shell').scan({ cwd: cache, absolute: true }))).sort().at(-1) });
const rows: string[] = [];
let bad = 0;
if (SHOTS) mkdirSync(SHOTS, { recursive: true });
for (const scheme of ['light', 'dark'] as const) {
	for (const [w, h] of SIZES) {
		const ctx = await browser.newContext({ viewport: { width: w, height: h }, colorScheme: scheme, deviceScaleFactor: 1 });
		const page = await ctx.newPage();
		await page.goto(BASE + '/');
		const st = await page.evaluate(async () => (await fetch('/api/v1/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ name: 'admin', password: 'adminadminadmin' }) })).status);
		if (st !== 200) throw new Error(`login failed ${st}`);
		let errors = 0;
		page.on('console', (m) => m.type() === 'error' && (errors++, console.log('  console.error:', m.text().slice(0, 160))));
		page.on('pageerror', (e) => (errors++, console.log('  pageerror:', e.message.slice(0, 160))));
		for (const path of PAGES) {
			errors = 0;
			await page.goto(BASE + path, { waitUntil: 'load' });
			await page.waitForTimeout(1200);
			const probe = () => page.evaluate(() => { const e = document.scrollingElement!; return { sh: e.scrollHeight, ih: innerHeight, sw: e.scrollWidth, iw: innerWidth }; });
			const check = async (label: string) => {
				const m = await probe();
				const ok = m.sh <= m.ih && m.sw <= m.iw && errors === 0;
				if (!ok) bad++;
				rows.push(`${scheme.padEnd(5)} ${w}x${h} ${label.padEnd(26)} h ${m.sh}/${m.ih}  w ${m.sw}/${m.iw}  err ${errors}  ${ok ? 'ok' : 'FAIL'}`);
			};
			await check(path);
			if (SHOTS && scheme === 'light' && shotNames[path] && (w === 1440 || w === 390)) await page.screenshot({ path: `${SHOTS}/${shotNames[path]}-${w}.jpg`, type: 'jpeg', quality: 70 });
			if (SHOTS && scheme === 'dark' && shotNames[path] === 'overview' && w === 1440) await page.screenshot({ path: `${SHOTS}/overview-dark-${w}.jpg`, type: 'jpeg', quality: 70 });
			if (w === 390) {
				const tabs = await page.locator('[role=tab]').all();
				for (const [i, t] of tabs.entries()) {
					if (i === 0) continue;
					const name = (await t.innerText()).trim().split('\n')[0];
					await t.click();
					await page.waitForTimeout(500);
					await check(`${path} [${name}]`);
					if (SHOTS && scheme === 'light' && path === '/settings') await page.screenshot({ path: `${SHOTS}/system-${name.toLowerCase()}-390.jpg`, type: 'jpeg', quality: 70 });
				}
			}
		}
		await ctx.close();
	}
}
await browser.close();
console.log(rows.join('\n'));
console.log(bad ? `\n${bad} FAILED` : `\nall ${rows.length} checks passed`);
process.exit(bad ? 1 : 0);
