// UI stress audit: every page x size x theme, reports layout problems with precise offenders.
//   MOCK_PORT=7951 bun scripts/mock-api.ts &  CK_DASH=7951 bun run dev -- --port 5191 &
//   BASE=http://localhost:5191 bun scripts/ui-stress.ts
// env: BASE, SCENARIO (label), SHOTS=<dir> (jpeg q70), PAGES=/a,/b?x=1 (comma list), SIZES=360x740,1440x900, PW, CHROME
// categories: 1 hscroll, 2 clipped text, 3 text overlap, 4 offscreen-x, 5 svg text overlap, 6 console errors,
//             7 timing (settle > 3000 ms / long tasks > 200 ms), 8 tap targets < 24px (phone widths <= 480)
// exit 1 if any of 1,2,3,6.
import { mkdirSync } from 'node:fs';
const { chromium } = (await import(process.env.PW ?? `${process.env.HOME}/.atlas-shot/node_modules/playwright`)) as typeof import('playwright');

const BASE = process.env.BASE ?? 'http://localhost:5191';
const SCENARIO = process.env.SCENARIO ?? 'default';
const SHOTS = process.env.SHOTS;
const PAGES = (process.env.PAGES?.split(',').map((s) => s.trim()).filter(Boolean)) ?? ['/', '/nodes', '/map', '/live', '/jobs', '/jobs/new', '/jobs/1', '/network', '/heads', '/policies', '/events', '/events?view=logs', '/experiments', '/sim', '/alerts', '/advisor', '/report', '/settings', '/users', '/wall', '/compare?jobs=1,2'];
const SIZES = (process.env.SIZES?.split(',') ?? ['360x740', '390x844', '768x1024', '1024x768', '1440x900', '1920x1080', '2940x1474']).map((s) => s.split('x').map(Number) as [number, number]);
const CATS = ['hscroll', 'clipped', 'overlap', 'offscreen', 'svgtext', 'errors', 'timing', 'tap'] as const;
type Cat = (typeof CATS)[number];
type Finding = { cat: Cat; msg: string };

// runs inside the page
function audit(phone: boolean) {
	const out: { cat: string; msg: string }[] = [];
	const csCache = new Map<Element, CSSStyleDeclaration>();
	const cs = (e: Element) => { let c = csCache.get(e); if (!c) csCache.set(e, (c = getComputedStyle(e))); return c; };
	const path = (el: Element) => {
		const parts: string[] = [];
		for (let e: Element | null = el; e && e !== document.body && parts.length < 4; e = e.parentElement) {
			let s = e.tagName.toLowerCase();
			if (e.id) { parts.unshift(s + '#' + e.id); break; }
			const cls = (typeof e.className === 'string' ? e.className : '').split(/\s+/).filter((c) => c && !/[:\[\]\/]/.test(c)).slice(0, 2);
			if (cls.length) s += '.' + cls.join('.');
			parts.unshift(s);
		}
		return parts.join(' > ');
	};
	const own = (el: Element) => { let t = ''; for (const n of el.childNodes) if (n.nodeType === 3) t += n.textContent; return t.replace(/\s+/g, ' ').trim(); };
	const snip = (s: string) => JSON.stringify(s.length > 40 ? s.slice(0, 40) + '…' : s);
	const R = (r: DOMRect) => `${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.width)}x${Math.round(r.height)}`;
	// rect clipped by overflow ancestors; null if nothing visible
	const vis = (el: Element, r0?: DOMRect) => {
		const r = r0 ?? el.getBoundingClientRect();
		let l = r.left, t = r.top, rr = r.right, b = r.bottom;
		if (rr - l <= 0 || b - t <= 0) return null;
		const c = cs(el);
		if (c.visibility === 'hidden' || c.display === 'none' || +c.opacity === 0) return null;
		if ((c.clip !== 'auto' && c.clip !== '') || (c.clipPath !== 'none' && c.clipPath !== '')) return null; // sr-only
		if (el.closest('details:not([open])') && !el.closest('summary')) return null;
		if (c.overflowX !== 'visible' || c.overflowY !== 'visible') { l = Math.max(l, r.left); rr = Math.min(rr, r.right); t = Math.max(t, r.top); b = Math.min(b, r.bottom); }
		if (c.position !== 'fixed') {
			for (let p = el.parentElement; p && p !== document.body && p !== document.documentElement; p = p.parentElement) {
				const pc = cs(p);
				if (pc.display === 'none') return null;
				if (pc.overflowX !== 'visible' || pc.overflowY !== 'visible') {
					const pr = p.getBoundingClientRect();
					l = Math.max(l, pr.left); t = Math.max(t, pr.top); rr = Math.min(rr, pr.right); b = Math.min(b, pr.bottom);
					if (rr <= l || b <= t) return null;
				}
			}
		}
		return { l, t, r: rr, b };
	};
	const hiddenAria = (el: Element) => !!el.closest('[aria-hidden=true], [inert]');
	const all = [...document.body.querySelectorAll('*')].filter((e) => !(e instanceof SVGElement) && !['SCRIPT', 'STYLE', 'NOSCRIPT'].includes(e.tagName));

	// 1 horizontal page overflow
	const se = document.scrollingElement!;
	if (se.scrollWidth > se.clientWidth) {
		let worst = '';
		for (const e of all) { const r = e.getBoundingClientRect(); if (r.right > se.clientWidth + 1 && vis(e)) { worst = `${path(e)} right=${Math.round(r.right)}`; break; } }
		out.push({ cat: 'hscroll', msg: `scrollWidth ${se.scrollWidth} > clientWidth ${se.clientWidth}${worst ? ' first: ' + worst : ''}` });
	}

	// leaves with own text
	const leaves: { el: Element; box: { l: number; t: number; r: number; b: number }; text: string }[] = [];
	for (const e of all) {
		const t = own(e);
		if (!t) continue;
		const range = document.createRange(); range.selectNodeContents(e);
		// text-node union rect (excludes child elements)
		let l = 1e9, tp = 1e9, r = -1e9, b = -1e9;
		for (const n of e.childNodes) if (n.nodeType === 3 && n.textContent!.trim()) { range.selectNodeContents(n); for (const q of range.getClientRects()) { if (q.width < 1 || q.height < 1) continue; l = Math.min(l, q.left); tp = Math.min(tp, q.top); r = Math.max(r, q.right); b = Math.max(b, q.bottom); } }
		if (r < l) continue;
		const v = vis(e, new DOMRect(l, tp, r - l, b - tp));
		if (!v || hiddenAria(e)) continue;
		leaves.push({ el: e, box: v, text: t });
	}

	// 2 clipped text (overflow visible but content spills)
	for (const { el, text } of leaves) {
		const c = cs(el);
		if (c.overflowX !== 'visible' || c.overflowY !== 'visible' || c.display === 'inline' || c.display === 'contents' || c.display.startsWith('table')) continue;
		const h = el as HTMLElement;
		if (h.clientWidth > 0 && (h.scrollWidth > h.clientWidth + 1 || h.scrollHeight > h.clientHeight + 1))
			out.push({ cat: 'clipped', msg: `${path(el)} ${snip(text)} sw ${h.scrollWidth}/${h.clientWidth} sh ${h.scrollHeight}/${h.clientHeight}` });
	}

	// 3 overlapping text boxes within the same card/section
	const groups = new Map<Element, typeof leaves>();
	for (const lf of leaves.slice(0, 3000)) {
		const g = lf.el.closest('section, article, li, tr, [class*=card], [class*=panel]') ?? document.body;
		(groups.get(g) ?? groups.set(g, []).get(g)!).push(lf);
	}
	const overlay = (e: Element) => { for (let p: Element | null = e; p && p !== document.body; p = p.parentElement) { const pos = cs(p).position; if (pos === 'absolute' || pos === 'fixed') return p; } return null; };
	let ov = 0;
	for (const g of groups.values()) {
		for (let i = 0; i < g.length; i++) for (let j = i + 1; j < g.length; j++) {
			const a = g[i], b = g[j];
			if (a.el.contains(b.el) || b.el.contains(a.el)) continue;
			const w = Math.min(a.box.r, b.box.r) - Math.max(a.box.l, b.box.l), h = Math.min(a.box.b, b.box.b) - Math.max(a.box.t, b.box.t);
			if (w <= 2 || h <= 2) continue;
			const oa = overlay(a.el);
			if (oa && oa === overlay(b.el)) continue;
			if (ov++ < 8) out.push({ cat: 'overlap', msg: `${path(a.el)} ${snip(a.text)} x ${path(b.el)} ${snip(b.text)} by ${Math.round(w)}x${Math.round(h)}` });
		}
	}
	if (ov > 8) out.push({ cat: 'overlap', msg: `... +${ov - 8} more overlapping pairs` });

	// 4 elements outside viewport horizontally and not in a scroll container
	const iw = innerWidth;
	let off = 0;
	for (const e of all) {
		if (e.children.length || hiddenAria(e)) continue;
		const r = e.getBoundingClientRect();
		if (r.width < 1 || (r.right <= iw + 1 && r.left >= -1)) continue;
		const v = vis(e);
		if (!v || (v.r <= iw + 1 && v.l >= -1)) continue;
		if (off++ < 5) out.push({ cat: 'offscreen', msg: `${path(e)} ${snip(own(e))} ${R(r)} (viewport ${iw})` });
	}
	if (off > 5) out.push({ cat: 'offscreen', msg: `... +${off - 5} more` });

	// 5 svg text overlaps
	for (const svg of document.querySelectorAll('svg')) {
		const ts = [...svg.querySelectorAll('text')].filter((t) => t.textContent?.trim() && vis(t) && !t.closest('[aria-hidden=true]'));
		if (ts.length > 600) continue; // ponytail: skip pathological svgs, O(n^2)
		let n = 0, first = '';
		const rs = ts.map((t) => t.getBoundingClientRect());
		for (let i = 0; i < ts.length; i++) for (let j = i + 1; j < ts.length; j++) {
			const w = Math.min(rs[i].right, rs[j].right) - Math.max(rs[i].left, rs[j].left), h = Math.min(rs[i].bottom, rs[j].bottom) - Math.max(rs[i].top, rs[j].top);
			if (w > 1 && h > 1) { if (!n) first = `${snip(ts[i].textContent!.trim())} x ${snip(ts[j].textContent!.trim())}`; n++; }
		}
		if (n) out.push({ cat: 'svgtext', msg: `${path(svg)} ${n} overlapping text pairs of ${ts.length}, e.g. ${first}` });
	}

	// 8 tap targets
	if (phone) {
		const small: string[] = [];
		for (const e of document.querySelectorAll('button, a[href], input:not([type=hidden]), select, textarea, [role=button], [role=tab]')) {
			const r = e.getBoundingClientRect();
			if (r.width <= 1 || r.height <= 1) continue;
			if ((r.width < 24 || r.height < 24) && vis(e) && !hiddenAria(e)) small.push(`${path(e)} ${snip((e.textContent ?? '').trim() || e.getAttribute('aria-label') || '')} ${Math.round(r.width)}x${Math.round(r.height)}`);
		}
		if (small.length) out.push({ cat: 'tap', msg: `${small.length} under 24x24: ${small.slice(0, 3).join(' | ')}` });
	}
	return out;
}

const cache = `${process.env.HOME}/Library/Caches/ms-playwright`;
const browser = await chromium.launch({ executablePath: process.env.CHROME ?? (await Array.fromAsync(new Bun.Glob('chromium_headless_shell-*/chrome-headless-shell-mac-arm64/chrome-headless-shell').scan({ cwd: cache, absolute: true }))).sort().at(-1) });
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

const t0 = Date.now();
const perPage = new Map<string, Record<Cat, number>>();
const offenders = new Map<string, number>(); // "cat|msg" -> combos
let combos = 0, dirty = 0, fatal = 0;
const blank = () => Object.fromEntries(CATS.map((c) => [c, 0])) as Record<Cat, number>;

for (const scheme of ['light', 'dark'] as const) {
	for (const [w, h] of SIZES) {
		const ctx = await browser.newContext({ viewport: { width: w, height: h }, colorScheme: scheme, deviceScaleFactor: 1 });
		await ctx.addInitScript(() => {
			(window as any).__lt = [];
			try { new PerformanceObserver((l) => { for (const e of l.getEntries()) (window as any).__lt.push(Math.round(e.duration)); }).observe({ type: 'longtask', buffered: true }); } catch {}
		});
		const page = await ctx.newPage();
		await page.goto(BASE + '/');
		const st = await page.evaluate(async () => (await fetch('/api/v1/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ name: 'admin', password: 'adminadminadmin' }) })).status).catch(() => 0);
		if (st !== 200 && combos === 0) console.log(`(login returned ${st}; continuing, mock probably in open mode)`);
		let errs: string[] = [];
		page.on('console', (m) => m.type() === 'error' && errs.push('console.error: ' + m.text().slice(0, 100) + ' ' + m.location().url.replace(BASE, '')));
		page.on('pageerror', (e) => errs.push('pageerror: ' + e.message.slice(0, 140)));
		for (const path of PAGES) {
			errs = [];
			const found: Finding[] = [];
			try {
				const s = Date.now();
				await page.goto(BASE + path, { waitUntil: 'load' });
				const loaded = Date.now() - s;
				// pages with SSE/polling never go idle: then settle = load time (not flagged as slow by the idle wait itself)
				const idle = await page.waitForLoadState('networkidle', { timeout: 3500 }).then(() => true, () => false);
				const settle = idle ? Date.now() - s : loaded;
				await page.waitForTimeout(600);
				const lt: number[] = await page.evaluate(() => (window as any).__lt);
				const longs = lt.filter((d) => d > 200);
				if (settle > 3000 || longs.length) found.push({ cat: 'timing', msg: `settle ${settle} ms${settle > 3000 ? ' (SLOW)' : ''}${longs.length ? `, ${longs.length} long tasks >200ms (max ${Math.max(...longs)})` : ''}` });
				for (const f of await page.evaluate(audit, w <= 480)) found.push(f as Finding);
				for (const e of errs.slice(0, 4)) found.push({ cat: 'errors', msg: e });
				if (errs.length > 4) found.push({ cat: 'errors', msg: `... +${errs.length - 4} more` });
				if (SHOTS) await page.screenshot({ path: `${SHOTS}/${SCENARIO}-${path.replace(/[^a-z0-9]+/gi, '_') || 'home'}-${w}-${scheme}.jpg`, type: 'jpeg', quality: 70 });
			} catch (e) {
				found.push({ cat: 'errors', msg: 'audit crashed: ' + String(e).slice(0, 160) });
				fatal++;
			}
			combos++;
			const row = perPage.get(path) ?? perPage.set(path, blank()).get(path)!;
			for (const f of found) {
				row[f.cat]++;
				const k = `${f.cat}|${f.msg.replace(/\d+ ms|max \d+|sw \d+\/\d+|sh \d+\/\d+|by \d+x\d+|\d+x\d+|settle \d+/g, '#')}`;
				offenders.set(k, (offenders.get(k) ?? 0) + 1);
			}
			if (found.length) {
				dirty++;
				console.log(`${scheme.padEnd(5)} ${w}x${h} ${path}`);
				for (const f of found) console.log(`    [${f.cat}] ${f.msg}`);
			}
		}
		await ctx.close();
	}
}
await browser.close();

console.log(`\n== summary (${SCENARIO}) ${combos} combos, ${dirty} with problems, ${((Date.now() - t0) / 1000).toFixed(0)} s ==`);
console.log('page'.padEnd(22) + CATS.map((c) => c.slice(0, 9).padStart(10)).join(''));
const totals = blank();
for (const [p, r] of perPage) {
	for (const c of CATS) totals[c] += r[c];
	if (CATS.some((c) => r[c])) console.log(p.padEnd(22) + CATS.map((c) => String(r[c] || '.').padStart(10)).join(''));
}
console.log('TOTAL'.padEnd(22) + CATS.map((c) => String(totals[c]).padStart(10)).join(''));
for (const c of CATS) {
	const top = [...offenders].filter(([k]) => k.startsWith(c + '|')).sort((a, b) => b[1] - a[1]).slice(0, 5);
	if (top.length) console.log(`\ntop ${c}:\n` + top.map(([k, n]) => `  x${n} ${k.slice(c.length + 1)}`).join('\n'));
}
process.exit(totals.hscroll || totals.clipped || totals.overlap || totals.errors || fatal ? 1 : 0);
