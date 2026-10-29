/* Watch review workspace. Browser-native, no build step. Docs: docs/REVIEW_UI.md */
(() => {
  "use strict";
  const { Timeline, Rail, fmt: fmtT } = window.WatchTimeline;
  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const qs = new URLSearchParams(location.search);
  const FIX = qs.get("fixture") === "1";
  const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
  const sec = (us) => (us / 1e6).toFixed(3) + " s";
  const utcStr = (us) => new Date(us / 1000).toISOString().replace("T", " ").slice(0, 23) + " UTC";
  const CH = 30e6; // observation chunk
  const VERS = ["unreviewed", "reviewed", "confirmed", "rejected"];

  const S = {
    run: null, media: {}, lanes: [], laneOf: {}, matches: [], byId: {}, list: [], cur: null, curLane: 0, view: "orig",
    ov: { boxes: 1, labels: 1, conf: 1, rels: 1, tracks: 0, zones: 0 }, edit: {}, sel: { title: "Review selection", notes: "", items: [] }, review: null,
    dirty: false, conflict: null, saveMsg: "", exp: null, offUs: 0, loop: false, req: null, act: null, frameUs: {}, models: null, mode: "none", loaded: 0,
    fx: null, fxReviews: {}, fxConflict: false, wantPlay: false, filter: { text: "", ver: "", src: "", conf: 0, group: false, sort: "time" },
  };
  window.__review = S; // test hook: read-only inspection from the console

  // ---------- API
  async function api(method, path, body) {
    if (FIX) { const r = fixApi(method, path, body); if (r !== undefined) return r; }
    const r = await fetch(path, { method, headers: body ? { "Content-Type": "application/json" } : undefined, body: body ? JSON.stringify(body) : undefined, credentials: "same-origin" });
    let j = null; try { j = await r.json(); } catch (_) {}
    if (!r.ok) {
      let e = j && j.error; if (typeof e === "string") e = { code: "http_" + r.status, message: e };
      e = e || { code: "http_" + r.status, message: r.statusText || "Request failed" };
      throw Object.assign(new Error(e.message), { api: e, status: r.status });
    }
    return j;
  }
  const errText = (e) => (e && e.api ? `${e.api.code}: ${e.api.message}${e.api.correlation_id ? " (" + e.api.correlation_id + ")" : ""}` : (e && e.message) || String(e));

  // ---------- fixture (TEST DATA)
  function rng(seed) { let s = (seed * 2654435761) >>> 0 || 1; return () => { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s / 4294967296; }; }
  function hashStr(s) { let h = 2166136261; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); } return h >>> 0; }
  function svgThumb(i, label) {
    const r = rng(i + 91), c = ["#326E6E", "#CE7D36", "#556", "#4a5"][i % 4];
    const x = 80 + r() * 300, y = 120 + r() * 120;
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="640" height="480"><rect width="640" height="480" fill="${c}"/><rect y="300" width="640" height="180" fill="#222"/><rect x="${x}" y="${y}" width="90" height="230" fill="#ddd" opacity=".85"/><text x="14" y="34" font-family="monospace" font-size="26" fill="#000">TEST DATA ${esc(label)} #${i + 1}</text></svg>`;
    return "data:image/svg+xml;utf8," + encodeURIComponent(svg);
  }
  function genFixture(fx) {
    for (const m of fx.media) S.media[m.id] = m;
    const lanes = fx.lanes.map((l) => {
      const med = S.media[l.media_id], g = l.gen || { n: 0, seed: 1 }, r = rng(g.seed), out = [];
      if (g.still) {
        for (let i = 0; i < g.n; i++) out.push(fxMatch(l, `m_${l.media_id}_${i}`, i * 1000, i * 1000 + 1000, i * 1000, i * 1000 + 1000, r, true, svgThumb(i, l.label)));
      } else if (g.n) {
        const D = l.duration_us || 600e6, ctx = 3e6; let t = ctx + r() * 1e6, prevEnd = 0;
        const gap = (D - 2 * ctx) / g.n;
        for (let i = 0; i < g.n; i++) {
          const dur = Math.round(g.dur[0] + Math.pow(r(), 3) * (g.dur[1] - g.dur[0]));
          let s = Math.max(Math.round(i * gap + ctx + r() * Math.max(0, gap - dur - 2 * ctx) * .9), prevEnd + 2 * ctx);
          if (s + dur + ctx > D) break;
          out.push(fxMatch(l, `m_${l.media_id}_${i}`, s, s + dur, Math.max(0, s - ctx), Math.min(D, s + dur + ctx), r, false, null, med));
          prevEnd = s + dur;
        }
      }
      return Object.assign({}, l, { matches: out });
    });
    return { lanes, gaps: fx.gaps };
  }
  function fxMatch(l, id, s, e, cs, ce, r, still, thumb, med) {
    const v = r(), conf = Math.round((.4 + r() * .58) * 100) / 100;
    return { id, query_run_id: "qr_fixture", source_id: l.source_id, media_id: l.media_id, clock: l.clock, start_us: s, end_us: e, context_start_us: cs, context_end_us: ce,
      capture_start_utc_us: med && med.capture_start_utc_us != null ? med.capture_start_utc_us + s : null, thumbnail: thumb, observation_ids: [`obs_${id}_0`], detection_ids: [`det_${id}_0`],
      relation_ids: [], distinct_tracks: 1 + Math.floor(r() * 3), max_confidence: conf, verification: v < .04 ? "confirmed" : v < .08 ? "reviewed" : v < .1 ? "rejected" : "unreviewed", still };
  }
  const PAL = { person: "#C5CFCB", "cell phone": "#CE7D36", bicycle: "#7fb7b7", car: "#d9b36a" };
  function fxObs(mid, a, b) {
    const med = S.media[mid], out = [], step = S.fx.observations.step_us, lane = S.laneOf[mid];
    if (!lane) return out;
    if (med && med.still) {
      for (const m of lane.matches) if (m.media_id === mid && m.start_us >= a && m.start_us < b) {
        const r = rng(hashStr(m.id)), oid = m.observation_ids[0], dets = [], rels = [];
        for (let k = 0; k < m.distinct_tracks; k++) { const x = .1 + r() * .5, y = .25 + r() * .3; dets.push({ detection_id: `${m.detection_ids[0]}_${k}`, observation_id: oid, class: "person", confidence: Math.round((m.max_confidence - .04 * k) * 100) / 100, bbox: [x, y, x + .14, y + .5], model_version: "fixture", track_id: null, provenance: "model" }); }
        out.push({ observation: { observation_id: oid, media_id: mid, media_time_us: m.start_us }, detections: dets, relations: rels });
      }
      return out;
    }
    const arr = lane.matches;
    for (let t = Math.ceil(a / step) * step; t < b; t += step) {
      let i = 0, lo = 0, hi = arr.length; while (lo < hi) { const q = (lo + hi) >> 1; if (arr[q].context_start_us < t - lane.maxLen) lo = q + 1; else hi = q; }
      const dets = [], rels = [];
      for (i = lo; i < arr.length && arr[i].context_start_us <= t; i++) {
        const m = arr[i]; if (t > m.context_end_us) continue;
        const inside = t >= m.start_us && t <= m.end_us, h = hashStr(m.id), tt = t / 1e6;
        for (let k = 0; k < m.distinct_tracks; k++) {
          const ph = ((h >> (k * 3)) & 255) / 255, cx = .15 + .7 * (0.5 + 0.5 * Math.sin(tt * (.25 + .07 * k) + ph * 6.28)), cy = .62 - .08 * k, w = .1 + .02 * k, hh = .34;
          const pid = `${m.id}_p${k}`, tr = `tr_${(h % 997)}_${k}`;
          dets.push({ detection_id: `det_${pid}_${t}`, observation_id: `obs_${m.id}_${t}`, class: "person", confidence: Math.round(Math.max(.2, (inside ? m.max_confidence : m.max_confidence * .7) - .04 * k) * 100) / 100, bbox: [cx - w / 2, cy - hh / 2, cx + w / 2, cy + hh / 2], model_version: "yolov5n-fixture", track_id: tr, provenance: "model" });
          if (k === 0 && h % 2 === 0) {
            dets.push({ detection_id: `det_${pid}_ph_${t}`, observation_id: `obs_${m.id}_${t}`, class: "cell phone", confidence: .55, bbox: [cx + w / 2 - .02, cy - .05, cx + w / 2 + .03, cy + .02], model_version: "yolov5n-fixture", track_id: null, provenance: "model" });
            rels.push({ relation_id: `rel_${pid}_${t}`, observation_id: `obs_${m.id}_${t}`, subject_detection_id: `det_${pid}_ph_${t}`, subject_track_id: null, predicate: "near", object_detection_id: `det_${pid}_${t}`, object_track_id: tr, object_class: "person", confidence: .5, module_version: "bbox-proximity", method: "bbox-proximity" });
          }
        }
      }
      if (dets.length) out.push({ observation: { observation_id: `obs_${mid}_${t}`, media_id: mid, media_time_us: t }, detections: dets, relations: rels });
    }
    return out;
  }
  const plain = (m) => Object.fromEntries(Object.entries(m).filter(([k]) => k[0] !== "_" && k !== "pk" && k !== "si"));
  function fixApi(method, path, body) {
    const u = new URL(path, location.origin), p = u.pathname, q = u.searchParams;
    const done = (v) => Promise.resolve(v), fail = (code, message, status) => Promise.reject(Object.assign(new Error(message), { api: { code, message, retryable: false }, status }));
    let m;
    if ((m = p.match(/^\/api\/v1\/media\/([^/]+)\/observations$/))) return done(fxObs(decodeURIComponent(m[1]), +q.get("start_us"), +q.get("end_us")));
    if ((m = p.match(/^\/api\/v1\/media\/([^/]+)$/))) return S.media[decodeURIComponent(m[1])] ? done(S.media[decodeURIComponent(m[1])]) : fail("not_found", "media not found", 404);
    if (p === "/api/v1/models") return done([]);
    if (p === "/api/v1/reviews" && method === "POST") { const r = { id: "rev_fx_" + Date.now().toString(36), version: 1, title: body.title, items: body.items, notes: body.notes || "", actor: null, export: "none", updated_utc_us: Date.now() * 1000 }; S.fxReviews[r.id] = r; return done(JSON.parse(JSON.stringify(r))); }
    if ((m = p.match(/^\/api\/v1\/reviews\/([^/]+)\/manifest$/))) {
      const r = S.fxReviews[m[1]]; if (!r) return fail("not_found", "review not found", 404);
      return done({ _test_data: "TEST DATA - fixture manifest, not evidence", review_id: r.id, review_version: r.version, query: S.run, items: r.items.map((it) => ({ ...it, match: plain(S.byId[it.match_id]), media: S.media[S.byId[it.match_id].media_id] })), generated_utc_us: Date.now() * 1000 });
    }
    if ((m = p.match(/^\/api\/v1\/reviews\/([^/]+)$/))) {
      const r = S.fxReviews[m[1]]; if (!r) return fail("not_found", "review not found", 404);
      if (method === "GET") return done(JSON.parse(JSON.stringify(r)));
      if (method === "PATCH") {
        if (S.fxConflict) { S.fxConflict = false; r.version += 1; r.notes = (r.notes || "") + " [edited elsewhere]"; return fail("conflict", `review is at version ${r.version}, you sent ${body.version}`, 409); }
        if (body.version !== r.version) return fail("conflict", `review is at version ${r.version}, you sent ${body.version}`, 409);
        Object.assign(r, { title: body.title ?? r.title, items: body.items ?? r.items, notes: body.notes ?? r.notes, version: r.version + 1, updated_utc_us: Date.now() * 1000 }); return done(JSON.parse(JSON.stringify(r)));
      }
    }
    if (p === "/api/v1/exports" && method === "POST") { const id = "exp_fx_" + Date.now().toString(36); S.fxExp = S.fxExp || {}; const rv = S.fxReviews[body.review_id]; S.fxExp[id] = { id, t0: Date.now(), fail: rv && /FAIL/i.test(rv.title), cancelled: false }; return done({ id, status: "queued", progress: 0 }); }
    if ((m = p.match(/^\/api\/v1\/exports\/([^/]+)\/cancel$/))) { const e = S.fxExp[m[1]]; e.cancelled = true; return done({ id: e.id, status: "cancelled", progress: 0 }); }
    if ((m = p.match(/^\/api\/v1\/exports\/([^/]+)$/))) {
      const e = S.fxExp[m[1]], pr = Math.min(1, (Date.now() - e.t0) / 7000);
      if (e.cancelled) return done({ id: e.id, status: "cancelled", progress: pr });
      if (e.fail && pr > .45) return done({ id: e.id, status: "failed", progress: .45, error: { code: "unavailable", message: "ffmpeg exited with status 1 (fixture failure)", retryable: true } });
      return done({ id: e.id, status: pr >= 1 ? "done" : pr > .05 ? "running" : "queued", progress: pr, output_url: null });
    }
    return fail("not_found", "fixture has no route " + p, 404);
  }

  // ---------- lanes
  function buildLanes(tl, extra) {
    const lanes = [], byKey = {}, add = (a) => {
      const med = S.media[a.media_id], still = !!(med && med.still) || !!a.still;
      const key = still ? "still:" + a.source_id : a.media_id || "live:" + a.source_id;
      if (a.label && /\.(mp4|mkv|mov|avi|webm|jpe?g|png)$/i.test(a.label)) { a = Object.assign({}, a, { recording: a.recording || a.label, label: a.source_id }); }
      let l = byKey[key];
      if (!l) {
        const cap = a.capture_start_utc_us ?? (med && med.capture_start_utc_us) ?? null;
        const clk = (a.clock && a.clock.kind) || (cap == null ? "media" : "utc");
        l = byKey[key] = { key, source_id: a.source_id, media_id: a.media_id || "", label: a.label || a.source_id, clock: clk, dur: a.duration_us ?? (med && med.duration_us) ?? 0, cap,
          availability: a.availability || (med && med.availability) || "unknown", recorded: a.recorded, still, matches: [], gaps: [], off: 0, recording: a.recording };
        const bits = [];
        if (a.recording) bits.push(a.recording); else if (cap != null) bits.push(utcStr(cap).slice(0, 19) + "Z"); else if (a.media_id) bits.push(a.media_id.slice(0, 18));
        l.sub = bits.join(" ");
        l.tag = l.recorded === false ? { text: "Not recorded. Live preview only.", warn: true } : l.availability === "missing" || l.availability === "expired" ? { text: "Media " + l.availability + ".", warn: true }
          : l.still ? { text: "Image collection." } : l.clock === "media" ? { text: "media time — capture clock unknown", warn: true } : null;
        lanes.push(l);
      }
      return l;
    };
    const seen = new Set();
    for (const a of tl.lanes || []) { const l = add(a); for (const m of a.matches || []) if (!seen.has(m.id)) { seen.add(m.id); l.matches.push(m); } }
    for (const m of extra) {
      if (seen.has(m.id)) continue; seen.add(m.id);
      const l = add({ source_id: m.source_id, media_id: m.media_id, clock: m.clock, still: m.still }); l.matches.push(m);
    }
    for (const g of tl.gaps || []) { const l = lanes.find((x) => x.media_id === g.media_id); if (l) l.gaps.push(g); }
    const rank = (l) => (l.recorded === false ? 2 : l.still ? 1 : 0);
    lanes.sort((a, b) => rank(a) - rank(b) || (a.source_id < b.source_id ? -1 : a.source_id > b.source_id ? 1 : (a.cap ?? 9e18) - (b.cap ?? 9e18) || (a.media_id < b.media_id ? -1 : 1)));
    return lanes;
  }
  function indexMatches() {
    S.byId = {}; S.laneOf = {};
    S.lanes.forEach((l, i) => { l.idx = i; for (const m of l.matches) { S.byId[m.id] = m; m._lane = l; } if (l.media_id) S.laneOf[l.media_id] = l; });
    S.matches = S.lanes.flatMap((l) => l.matches);
  }

  // ---------- results list (virtual)
  let rows = [], rowY = [], listH = 0;
  function applyFilter() {
    const f = S.filter, t = f.text.toLowerCase();
    let list = S.matches.filter((m) => (!f.ver || m.verification === f.ver) && (!f.src || m.source_id === f.src) && m.max_confidence >= f.conf && (!t || (m.id + " " + m.source_id + " " + m.media_id).toLowerCase().includes(t)));
    if (f.sort === "conf") list = list.slice().sort((a, b) => b.max_confidence - a.max_confidence);
    S.list = list;
    rows = []; rowY = []; let y = 0, last = null;
    for (const m of list) {
      if (f.group && f.sort === "time" && m._lane !== last) { last = m._lane; rows.push({ g: m._lane.label + " · " + (m._lane.sub || "") }); rowY[rows.length - 1] = y; y += 26; }
      rows.push({ m }); rowY[rows.length - 1] = y; y += 52;
    }
    listH = y; rowY.length = rows.length;
    $("rv-rescount").textContent = `${list.length.toLocaleString()} / ${S.matches.length.toLocaleString()}`;
    paintList(); railSeq.set(list, (m) => S.cur && S.cur.id === m.id);
    if ($("rv-list").dataset.empty !== (list.length ? "0" : "1")) $("rv-list").dataset.empty = list.length ? "0" : "1";
  }
  function rowLabel(m) { return `${esc(m._lane.label)} ${m.still ? "image" : fmtT(m.start_us) + "–" + fmtT(m.end_us)}`; }
  function paintList() {
    const box = $("rv-list"); let sz = box.firstElementChild;
    if (!sz) { box.innerHTML = '<div class="sizer"></div>'; sz = box.firstElementChild; }
    sz.style.height = listH + "px";
    if (!rows.length) { sz.innerHTML = `<p class="quiet" style="padding:12px">${S.matches.length ? "No matches pass these filters." : "No matches."}</p>`; return; }
    const top = box.scrollTop, h = Math.min(box.clientHeight || 400, 1200);
    let lo = 0, hi = rows.length; while (lo < hi) { const q = (lo + hi) >> 1; if (rowY[q] + 52 < top) lo = q + 1; else hi = q; }
    let html = "";
    for (let i = lo; i < rows.length && rowY[i] < top + h; i++) {
      const r = rows[i];
      if (r.g) { html += `<div class="rv-row group" style="top:${rowY[i]}px">${esc(r.g)}</div>`; continue; }
      const m = r.m, sel = S.cur && S.cur.id === m.id, inSel = S.sel.items.some((it) => it.match_id === m.id);
      html += `<div class="rv-row" role="option" id="row-${esc(m.id)}" data-id="${esc(m.id)}" aria-selected="${sel}" style="top:${rowY[i]}px"><div class="th" style="${m.thumbnail ? `background-image:url('${esc(m.thumbnail)}')` : ""}"></div><div><b><span class="vdot ${m.verification}"></span>${rowLabel(m)}</b><span>conf ${m.max_confidence.toFixed(2)} · ${m.distinct_tracks} trk${inSel ? " · in selection" : ""}</span></div></div>`;
    }
    sz.innerHTML = html;
  }

  // ---------- thumbnails (filmstrip). Lazy, visible only.
  const TH = { cache: new Map(), q: [], busy: 0, vids: {}, frame: 0 };
  function thumbTile(m, t, n) {
    const key = m.id + "|" + n; let e = TH.cache.get(key);
    if (!e) { e = { st: "wait", seen: performance.now(), n }; TH.cache.set(key, e); TH.q.push({ m, n, key, e }); if (TH.cache.size > 600) { const k = TH.cache.keys().next().value; TH.cache.delete(k); } pump(); }
    e.seen = performance.now();
    if (e.st !== "ok") return null;
    const a = e.samples; let lo = 0, hi = a.length; while (lo < hi) { const q = (lo + hi) >> 1; if (a[q].t_us < t) lo = q + 1; else hi = q; }
    const s = !lo ? a[0] : lo >= a.length ? a[a.length - 1] : (t - a[lo - 1].t_us < a[lo].t_us - t ? a[lo - 1] : a[lo]);
    return s ? { img: e.img, sx: s.x, sy: s.y, sw: e.tw, sh: e.th } : null;
  }
  function pump() {
    const max = FIX ? 1 : 4;
    while (TH.busy < max && TH.q.length) {
      const job = TH.q.pop(); // newest first: what is on screen now
      if (performance.now() - job.e.seen > 700) { TH.cache.delete(job.key); continue; }
      TH.busy++; job.e.st = "load";
      (FIX ? fixSprite(job) : realSprite(job)).then((r) => { Object.assign(job.e, r, { st: "ok" }); tl.redraw(); }).catch(() => { job.e.st = "err"; }).finally(() => { TH.busy--; pump(); });
    }
  }
  async function realSprite({ m, n }) { // server workaround: the end-inclusive sample tile is black, so it is dropped
    const step = Math.max(1, Math.floor((m.end_us - m.start_us) / n));
    const r = await api("GET", `/api/v1/media/${encodeURIComponent(m.media_id)}/thumbnails?start_us=${m.start_us}&end_us=${m.end_us}&step_us=${step}`);
    const img = await new Promise((ok, no) => { const i = new Image(); i.onload = () => ok(i); i.onerror = no; i.src = r.sprite; });
    return { img, tw: r.tile_w, th: r.tile_h, samples: (r.samples || []).filter((x, _, a) => a.length < 2 || x.t_us < m.end_us - 5000).sort((a, b) => a.t_us - b.t_us) };
  }
  async function fixSprite({ m, n }) {
    const med = S.media[m.media_id]; if (!med || !med.fixture_stream) throw new Error("no stream");
    const v = TH.vids[med.fixture_stream] || (TH.vids[med.fixture_stream] = Object.assign(document.createElement("video"), { muted: true, preload: "auto", src: med.fixture_stream }));
    if (v.readyState < 1) await new Promise((ok, no) => { v.onloadedmetadata = ok; v.onerror = no; });
    const tw = 71, th = 40, cv = document.createElement("canvas"); cv.width = tw * n; cv.height = th; const c = cv.getContext("2d"), samples = [];
    for (let i = 0; i < n; i++) {
      const t = m.start_us + (i + .5) / n * (m.end_us - m.start_us), vt = med.fixture_wrap_us ? (t % med.fixture_wrap_us) : t;
      await new Promise((ok) => { v.onseeked = ok; v.currentTime = Math.min(vt / 1e6, (v.duration || 1e9) - .05); });
      c.drawImage(v, i * tw, 0, tw, th); samples.push({ t_us: t, x: i * tw, y: 0 });
    }
    return { img: cv, tw, th, samples };
  }
  const stillImg = new Map();
  function stillTile(m) {
    if (!m.thumbnail) return null; let i = stillImg.get(m.id);
    if (!i) { i = new Image(); i.onload = () => tl.redraw(); i.src = m.thumbnail; stillImg.set(m.id, i); }
    return i.complete && i.naturalWidth ? i : null;
  }

  // ---------- timeline + rails
  const tip = $("rv-tip");
  const tl = new Timeline($("rv-tl"), {
    tile: thumbTile, still: stillTile,
    onPick: (m, li, t) => loadMatch(m, { seek: t == null ? m.start_us : t, user: true }),
    onSeek: (li, t, m) => { if (m) { if (!S.cur || S.cur.id !== m.id) loadMatch(m, { seek: t }); else seek(t); } else seekLane(li, t); },
    onLane: (li) => { const l = S.lanes[li]; if (l && l.matches[0]) loadMatch(l.matches[0], { seek: l.matches[0].start_us }); },
    onHandle: (kind, m, t, final) => handle(kind, m, t, final),
    onHover: (h, x, y) => {
      if (!h) { tip.classList.add("hide"); return; }
      tip.classList.remove("hide"); tip.style.left = Math.min(innerWidth - 310, x + 12) + "px"; tip.style.top = y + 14 + "px";
      tip.innerHTML = h.gap ? `Indexing gap ${fmtT(h.gap.start_us)}–${fmtT(h.gap.end_us)}<br>${esc(h.gap.reason || "not analyzed")}` : `${esc(h.m.id)}<br>${h.m.still ? "image" : fmtT(h.m.start_us, 1) + " – " + fmtT(h.m.end_us, 1)}<br>conf ${h.m.max_confidence.toFixed(2)} · ${h.m.verification}`;
    },
  });
  S.tl = tl;
  const railSeq = new Rail($("rv-seq"), {
    label: "Matched-only sequence", empty: "No matches.", w: (m) => clamp((m.end_us - m.start_us) / 1e6 * 6, 26, 120),
    pick: (m) => loadMatch(m, { seek: m.start_us, user: true }),
    draw(c, m, i, x, y, w, h, st) {
      const cur = S.cur && S.cur.id === m.id; c.fillStyle = m.verification === "rejected" ? st.col.bad : st.col.lead; c.globalAlpha = .5 + .5 * m.max_confidence; c.fillRect(x, y, w, h); c.globalAlpha = 1;
      c.strokeStyle = cur ? st.col.pop : "#0A0A0A"; c.lineWidth = cur ? 2 : 1; c.strokeRect(x + .5, y + .5, w - 1, h - 1); c.lineWidth = 1;
      if (st.focus) { c.strokeStyle = st.col.sup; c.strokeRect(x + 2, y + 2, w - 4, h - 4); }
      c.fillStyle = "#0A0A0A"; const ix = String(i + 1); c.fillText(ix, x + 4, y + h / 2); if (w > 50) { const iw = c.measureText(ix).width; if (w > iw + 48) c.fillText(fmtT(m.start_us), x + iw + 10, y + h / 2); }
      if (S.sel.items.some((it) => it.match_id === m.id)) { c.fillStyle = st.col.sup; c.fillRect(x, y, w, 3); }
    },
  });
  const railSel = new Rail($("rv-sel"), {
    label: "Selected clips", empty: "No clips selected. Press A to add the current match.", w: (it) => clamp((it.out_us - it.in_us) / 1e6 * 8, 78, 220),
    pick: (it) => { const m = S.byId[it.match_id]; if (m) loadMatch(m, { seek: it.in_us, user: true }); },
    reorder: (a, b) => { const [x] = S.sel.items.splice(a, 1); S.sel.items.splice(b, 0, x); markDirty(); refreshSel(); },
    remove: (it) => toggleItem(S.byId[it.match_id]),
    draw(c, it, i, x, y, w, h, st) {
      const m = S.byId[it.match_id], cur = S.cur && m && S.cur.id === m.id;
      c.fillStyle = "#1d2523"; c.fillRect(x, y, w, h); c.fillStyle = st.col.sup; c.fillRect(x, y, 4, h);
      c.strokeStyle = cur ? st.col.pop : st.col.line; c.lineWidth = cur ? 2 : 1; c.strokeRect(x + .5, y + .5, w - 1, h - 1); c.lineWidth = 1;
      if (st.focus) { c.strokeStyle = st.col.pop; c.strokeRect(x + 3, y + 3, w - 6, h - 6); }
      if (st.drag) { c.fillStyle = "rgba(197,207,203,.2)"; c.fillRect(x, y, w, h); }
      c.fillStyle = "#F5F5F5"; c.fillText(`${i + 1}. ${m ? m._lane.label : "?"}`, x + 10, y + 12); c.fillStyle = "#A3A3A3"; c.fillText(`${fmtT(it.in_us)}–${fmtT(it.out_us)} · ${it.verification}`, x + 10, y + h - 10);
    },
  });
  function refreshSel() {
    const map = {}; for (const it of S.sel.items) map[it.match_id] = it;
    tl.setItems(map); railSel.set(S.sel.items.filter((it) => S.byId[it.match_id])); railSeq.redraw(); paintList(); renderDetails();
  }

  // ---------- selection / review
  const getItem = (m) => S.sel.items.find((it) => it.match_id === m.id);
  function ensureItem(m) {
    let it = getItem(m);
    if (!it) { const e = S.edit[m.id]; it = { match_id: m.id, in_us: e ? e.cs : m.context_start_us, out_us: e ? e.ce : m.context_end_us, note: "", verification: m.verification }; S.sel.items.push(it); markDirty(); }
    return it;
  }
  function toggleItem(m) {
    if (!m) return; const i = S.sel.items.findIndex((it) => it.match_id === m.id);
    if (i >= 0) S.sel.items.splice(i, 1); else ensureItem(m);
    markDirty(); refreshSel();
  }
  function markDirty() { S.dirty = true; S.saveMsg = ""; }
  function setVerify(v) {
    const m = S.cur; if (!m) return; m.verification = v; const it = ensureItem(m); it.verification = v; markDirty(); refreshSel(); tl.redraw();
  }
  function handle(kind, m, t, final) {
    const dur = (m._lane.dur || m.context_end_us), e = S.edit[m.id] || (S.edit[m.id] = { cs: m.context_start_us, ce: m.context_end_us });
    if (kind === "cs") e.cs = clamp(Math.round(t), 0, m.start_us);
    else if (kind === "ce") e.ce = clamp(Math.round(t), m.end_us, dur);
    else { const it = ensureItem(m); if (kind === "in") it.in_us = clamp(Math.round(t), e.cs, it.out_us - 1e5); else it.out_us = clamp(Math.round(t), it.in_us + 1e5, e.ce); markDirty(); }
    const it = getItem(m); if (it) { it.in_us = clamp(it.in_us, e.cs, e.ce); it.out_us = clamp(it.out_us, e.cs, e.ce); }
    tl.setEdits(S.edit); if (it) tl.setItems(Object.fromEntries(S.sel.items.map((x) => [x.match_id, x])));
    seek(kind === "cs" ? e.cs : kind === "ce" ? e.ce : it && kind === "in" ? it.in_us : it ? it.out_us : m.start_us);
    if (final) { railSel.set(S.sel.items.filter((x) => S.byId[x.match_id])); renderDetails(); }
  }
  async function saveSel() {
    const body = { title: S.sel.title, items: S.sel.items, notes: S.sel.notes };
    S.saveMsg = "Saving…"; renderDetails();
    try {
      const r = S.review ? await api("PATCH", `/api/v1/reviews/${S.review.id}`, { version: S.review.version, ...body }) : await api("POST", "/api/v1/reviews", body);
      S.review = r; S.dirty = false; S.conflict = null; S.saveMsg = `Saved as version ${r.version}.`;
      const u = new URL(location.href); u.searchParams.set("review", r.id); history.replaceState(null, "", u);
    } catch (e) {
      if (e.status === 409 || (e.api && e.api.code === "conflict")) { S.conflict = { msg: errText(e), mine: body }; S.saveMsg = ""; } else S.saveMsg = "Save failed. " + errText(e);
    }
    renderDetails();
  }
  async function conflictReload() {
    try { const r = await api("GET", `/api/v1/reviews/${S.review.id}`); applyReview(r); S.conflict = null; S.dirty = false; S.saveMsg = `Loaded version ${r.version} (your unsaved edits were discarded).`; refreshSel(); } catch (e) { S.saveMsg = "Reload failed. " + errText(e); renderDetails(); }
  }
  async function conflictKeep() {
    try { const r = await api("GET", `/api/v1/reviews/${S.review.id}`); S.review = r; S.conflict = null; await saveSel(); } catch (e) { S.saveMsg = "Retry failed. " + errText(e); renderDetails(); }
  }
  function applyReview(r) {
    S.review = r; S.sel = { title: r.title, notes: r.notes || "", items: r.items.map((x) => ({ ...x })) };
    for (const it of S.sel.items) { const m = S.byId[it.match_id]; if (m) m.verification = it.verification; }
  }
  async function manifest() {
    if (S.dirty || !S.review) await saveSel(); if (!S.review || S.dirty) return;
    try {
      const j = await api("POST", `/api/v1/reviews/${S.review.id}/manifest`, {});
      const a = Object.assign(document.createElement("a"), { href: URL.createObjectURL(new Blob([JSON.stringify(j, null, 2)], { type: "application/json" })), download: `evidence-manifest-${S.review.id}.json` });
      document.body.appendChild(a); a.click(); a.remove(); S.saveMsg = "Evidence manifest downloaded.";
    } catch (e) { S.saveMsg = "Manifest failed. " + errText(e); }
    renderDetails();
  }
  let expTimer = 0;
  async function startExport() {
    if (S.dirty || !S.review) await saveSel(); if (!S.review || S.dirty) return;
    S.exp = { status: "queued", progress: 0 }; renderDetails();
    try { const r = await api("POST", "/api/v1/exports", { review_id: S.review.id, review_version: S.review.version, kind: "sequence", reencode: true }); S.exp = { ...r, id: r.id }; pollExport(); }
    catch (e) { S.exp = { status: "failed", error: { code: (e.api && e.api.code) || "error", message: errText(e) } }; renderDetails(); }
  }
  function pollExport() {
    clearTimeout(expTimer);
    expTimer = setTimeout(async () => {
      if (!S.exp || !S.exp.id || ["done", "failed", "cancelled"].includes(S.exp.status)) return;
      try { S.exp = { ...S.exp, ...(await api("GET", `/api/v1/exports/${S.exp.id}`)) }; } catch (e) { S.exp = { ...S.exp, status: "failed", error: { code: (e.api && e.api.code) || "error", message: errText(e) } }; }
      updateExp(); pollExport();
    }, 800);
  }
  async function cancelExport() { try { S.exp = { ...S.exp, ...(await api("POST", `/api/v1/exports/${S.exp.id}/cancel`, {})) }; } catch (e) { S.exp.error = { code: "cancel_failed", message: errText(e) }; } updateExp(); }
  function updateExp() {
    const box = $("ex-box"); if (!box) return; const x = S.exp; if (!x) { box.innerHTML = ""; return; }
    const run = x.status === "queued" || x.status === "running", pr = Math.round((x.progress || 0) * 100);
    const sig = x.status + "|" + JSON.stringify(x.error) + "|" + (x.output || x.output_url || "");
    if (box.dataset.sig === sig && run) { const pg = box.querySelector("progress"); if (pg) { pg.value = pr; pg.nextSibling.textContent = " " + pr + "% "; } return; }
    box.dataset.sig = sig;
    box.innerHTML = `<div class="rv-callout ${x.status === "failed" ? "err" : x.status === "done" ? "ok" : ""}" role="status"><b>Rendered export: ${esc(x.status)}</b>${run ? ` <progress max="100" value="${pr}"></progress> ${pr}% <button id="ex-cancel">Cancel</button>` : ""}
      ${x.error ? `<div>${typeof x.error === "string" ? esc(x.error) : esc(x.error.code) + ": " + esc(x.error.message)}</div>` : ""}${x.status === "done" ? `<div>${(x.output_url || x.output) ? `<a href="${esc(x.output_url || x.output)}" download>Download render</a>` : "Render finished."}</div>` : ""}
      <div class="quiet">Re-encoded cuts. Not stream copy, so keyframe alignment does not limit the in/out points.</div></div>`;
    const c = $("ex-cancel"); if (c) c.onclick = cancelExport;
  }

  // ---------- details panel
  function renderDetails() {
    const el = $("rv-det"), m = S.cur, ae = document.activeElement, fid = ae && el.contains(ae) ? ae.id : null, ss = fid && ae.selectionStart;
    const it = m && getItem(m), lane = m && m._lane, med = m && S.media[m.media_id], e = m && S.edit[m.id];
    let h = "";
    if (m) {
      const frameUrl = m.thumbnail || (FIX ? null : `/api/v1/media/${encodeURIComponent(m.media_id)}/frame?t_us=${m.start_us}`);
      h += `<div><h3>${esc(lane.label)} · ${m.still ? "image" : fmtT(m.start_us, 1) + "–" + fmtT(m.end_us, 1)}</h3>
        ${frameUrl ? `<img class="ev" alt="Evidence frame at the match in-point" src="${esc(frameUrl)}" onerror="this.style.display='none'">` : ""}</div>
        <dl><dt>match</dt><dd>${esc(m.id)}</dd><dt>media</dt><dd>${esc(m.media_id)}</dd><dt>media time</dt><dd>${m.still ? "image" : sec(m.start_us) + " → " + sec(m.end_us)}</dd>
        <dt>context</dt><dd>${m.still ? "n/a" : sec(e ? e.cs : m.context_start_us) + " → " + sec(e ? e.ce : m.context_end_us)}</dd>
        <dt>capture</dt><dd>${m.capture_start_utc_us != null ? esc(utcStr(m.capture_start_utc_us)) : "unknown capture clock"}</dd>
        <dt>confidence</dt><dd>${m.max_confidence.toFixed(2)} max</dd><dt>tracks</dt><dd>${m.distinct_tracks} distinct</dd><dt>evidence</dt><dd>${m.observation_ids.length} obs · ${m.detection_ids.length} det · ${m.relation_ids.length} rel</dd>
        ${med && med.timebase ? `<dt>timebase</dt><dd>${esc(med.timebase)}</dd>` : ""}</dl>
        <div><b>Observed in window</b><div class="rv-ev" id="ev-list" aria-live="off">loading…</div></div>
        <div><b>Decision</b><div class="rv-seg4" role="group" aria-label="Verification">${VERS.map((v) => `<button data-ver="${v}" aria-pressed="${m.verification === v}" title="${v}">${v}</button>`).join("")}</div></div>
        <div><label for="d-note"><b>Note</b></label><textarea id="d-note" placeholder="${it ? "" : "Typing adds this match to the selection"}">${esc(it ? it.note : "")}</textarea></div>
        <div><button id="d-toggle" aria-pressed="${!!it}">${it ? "In selection (A to remove)" : "Add to selection (A)"}</button></div>`;
    } else h += `<p class="quiet">${S.lanes.length ? "Select a strip or a result." : "Nothing loaded."}</p>`;
    h += `<hr style="border:0;border-top:1px solid var(--line);width:100%"><div><b>Review selection</b>
      <label class="sr-only" for="d-title">Title</label><input type="text" id="d-title" value="${esc(S.sel.title)}" style="margin-top:4px">
      <label for="d-rnotes" class="quiet" style="font-size:12px">Collection notes</label><textarea id="d-rnotes">${esc(S.sel.notes)}</textarea>
      <p class="mono quiet" style="font-size:11px;margin:4px 0">${S.sel.items.length} clip(s) · ${S.review ? "saved v" + S.review.version + " · " + esc(S.review.id) : "not saved"}${S.dirty ? " · UNSAVED CHANGES" : ""}</p>
      <div class="row"><button id="d-save">${S.review ? "Save (new version)" : "Save selection"}</button><button id="d-manifest">Evidence manifest (JSON)</button><button id="d-export">Render export</button>${FIX ? '<button id="d-fx409" title="Test hook: next save returns 409">Simulate 409</button>' : ""}</div>
      ${S.conflict ? `<div class="rv-callout err" role="alert"><b>Conflict: someone else changed this review.</b><div>${esc(S.conflict.msg)}</div><div class="row"><button id="c-reload">Reload latest (discard mine)</button><button id="c-keep">Keep mine, save on top</button></div></div>` : ""}
      ${S.saveMsg ? `<div class="rv-callout ${/fail/i.test(S.saveMsg) ? "err" : "ok"}" role="status">${esc(S.saveMsg)}</div>` : ""}<div id="ex-box"></div></div>`;
    el.innerHTML = h; el.className = "rv-det"; updateExp();
    el.querySelectorAll("[data-ver]").forEach((b) => (b.onclick = () => setVerify(b.dataset.ver)));
    const on = (id, f) => { const n = $(id); if (n) n.onclick = f; };
    on("d-toggle", () => toggleItem(m)); on("d-save", saveSel); on("d-manifest", manifest); on("d-export", startExport); on("c-reload", conflictReload); on("c-keep", conflictKeep);
    on("d-fx409", () => { S.fxConflict = true; S.saveMsg = "Fixture: the next save will return 409."; renderDetails(); });
    const note = $("d-note"); if (note) note.oninput = () => { ensureItem(m).note = note.value; markDirty(); };
    $("d-title").oninput = (ev) => { S.sel.title = ev.target.value; markDirty(); }; $("d-rnotes").oninput = (ev) => { S.sel.notes = ev.target.value; markDirty(); };
    if (fid && $(fid)) { $(fid).focus(); try { $(fid).setSelectionRange(ss, ss); } catch (_) {} }
    if (m) fillEvidence(m);
  }
  async function fillEvidence(m) {
    try {
      const a = m.still ? m.start_us : m.start_us, b = m.still ? m.start_us + 1 : m.end_us;
      let r = await api("GET", `/api/v1/media/${encodeURIComponent(m.media_id)}/observations?start_us=${a}&end_us=${b}`); r = Array.isArray(r) ? r : (r && r.observations) || [];
      if (!S.cur || S.cur.id !== m.id || !$("ev-list")) return;
      const lines = []; let n = 0;
      for (const o of r) for (const d of o.detections || []) { if (n++ > 150) break; lines.push(`<div>${m.still ? "" : fmtT(o.observation.media_time_us, 1) + " "}${esc(d.class)} ${d.confidence.toFixed(2)}${d.track_id ? " " + esc(d.track_id) : ""}</div>`); }
      for (const o of r) for (const x of o.relations || []) if (n++ < 160) lines.push(`<div>${fmtT(o.observation.media_time_us, 1)} ${esc(x.predicate)} ${esc(x.object_class)} ${x.confidence.toFixed(2)} (${esc(x.method)}, approximate)</div>`);
      $("ev-list").innerHTML = lines.join("") || '<div>No observations returned for this window.</div>';
    } catch (e) { if ($("ev-list")) $("ev-list").innerHTML = `<div>Could not load evidence: ${esc(errText(e))}</div>`; }
  }

  // ---------- player
  const vid = $("rv-video");
  const vNow = () => vid.currentTime * 1e6 + S.offUs;
  const tickUs = (med) => { const m = med && med.timebase && med.timebase.match(/^(\d+)\/(\d+)$/); return m ? 1e6 * (+m[1]) / (+m[2]) : 0; };
  function msg(title, text, bad) { const el = $("rv-msg"); el.className = "rv-msg" + (bad ? " bad" : ""); el.innerHTML = `<b>${esc(title)}</b><span>${esc(text)}</span>`; vid.style.visibility = "hidden"; $("rv-hud").textContent = ""; clearOverlay(); S.mode = "msg"; }
  function showVideo() { $("rv-msg").className = "rv-msg hide"; $("rv-sheet").classList.add("hide"); vid.style.visibility = "visible"; S.mode = "video"; }
  function streamUrl(med) { return FIX ? med.fixture_stream : `/api/v1/media/${encodeURIComponent(med.id)}/stream`; }
  async function mediaFor(id) {
    if (S.media[id]) return S.media[id];
    try { S.media[id] = await api("GET", `/api/v1/media/${encodeURIComponent(id)}`); } catch (e) { return null; } return S.media[id];
  }
  async function loadMatch(m, o) {
    o = o || {}; if (o.user && innerWidth <= 760 && $("rv").dataset.tab === "results") document.querySelector('#rv-mtabs [data-tab=player]').click();
    S.cur = m; S.curLane = m._lane.idx; tl.setCurrent(m, S.curLane); tl.scrollToMatch(m, S.curLane);
    paintList(); railSeq.redraw(); renderDetails(); revealInList(m);
    const u = new URL(location.href); u.searchParams.set("match", m.id); history.replaceState(null, "", u);
    const lane = m._lane;
    if (lane.recorded === false) { return msg("Historical footage was not recorded", `${lane.label} is a live source with only a preview. There is nothing to replay for this time.`, true); }
    if (m.still) return showSheet(m);
    const med = await mediaFor(m.media_id);
    if (S.cur !== m) return;
    if (!med) return msg("Media record unavailable", `The media record ${m.media_id} could not be loaded.`, true);
    if (med.availability !== "available" && med.availability !== "unknown") return msg("Media " + med.availability, `${med.uri || m.media_id} is ${med.availability}. Matches and evidence are still listed, but the original cannot be played.`, true);
    showVideo(); S.wantPlay = !!o.play;
    const url = streamUrl(med);
    if (vid.dataset.mid !== med.id) {
      vid.dataset.mid = med.id; vid.src = url; vid.load();
      await new Promise((ok) => { const done = () => { vid.removeEventListener("loadedmetadata", done); vid.removeEventListener("error", done); ok(); }; vid.addEventListener("loadedmetadata", done); vid.addEventListener("error", done); });
      if (vid.error) { vid.dataset.mid = ""; return msg("Playback failed", `The browser could not open ${url} (media error ${vid.error.code}${vid.error.message ? ": " + vid.error.message : ""}).`, true); }
    }
    if (S.cur !== m) return;
    seek(o.seek != null ? o.seek : m.start_us);
    if (S.wantPlay) vid.play().catch(() => {});
  }
  function seekLane(li, t) {
    const l = S.lanes[li]; if (!l || l.still) return;
    if (l.recorded === false) return;
    const m = l.matches.find((x) => x.context_start_us <= t && x.context_end_us >= t) || l.matches[0];
    if (m && (!S.cur || S.cur.media_id !== l.media_id)) { loadMatch(m, { seek: t }); return; }
    S.curLane = li; seek(t);
  }
  function seek(tUs) {
    const med = S.media[vid.dataset.mid]; if (!med || S.mode !== "video") return;
    const tick = tickUs(med); let t = tUs; if (tick) t = Math.round(t / tick) * tick;
    if (med.fixture_wrap_us) { S.offUs = t - (t % med.fixture_wrap_us); }
    else S.offUs = 0;
    const vt = (t - S.offUs) / 1e6; S.req = { t, vt, at: performance.now() };
    vid.currentTime = clamp(vt, 0, isFinite(vid.duration) ? vid.duration : vt); S.ovDirty = 1;
    tl.setPlayhead(S.curLane, t, true);
  }
  vid.addEventListener("seeked", () => {
    const med = S.media[vid.dataset.mid], r = S.req; if (!r) return;
    const act = vid.currentTime, d = (act - r.vt) * 1000;
    $("rv-seekinfo").textContent = `requested ${r.vt.toFixed(6)} s · actual ${act.toFixed(6)} s · delta ${d.toFixed(1)} ms · timebase ${med && med.timebase ? med.timebase : "unknown"} · frame ${frameUs(med) ? (frameUs(med) / 1000).toFixed(1) + " ms " + (S.frameUs[med.id] ? "measured" : "assumed") : "unknown"} · browser seeking is not guaranteed frame-exact`;
    S.ovDirty = 1;
  });
  vid.addEventListener("error", () => { if (vid.dataset.mid && S.mode === "video") msg("Playback failed", `Media error ${vid.error ? vid.error.code : "?"}${vid.error && vid.error.message ? ": " + vid.error.message : ""}.`, true); });
  vid.addEventListener("play", () => { $("b-play").innerHTML = "&#10074;&#10074;"; $("b-play").setAttribute("aria-label", "Pause"); });
  vid.addEventListener("pause", () => { $("b-play").innerHTML = "&#9654;"; $("b-play").setAttribute("aria-label", "Play"); });
  function frameUs(med) { return med ? S.frameUs[med.id] || (med.fixture_fps ? 1e6 / med.fixture_fps : 1e6 / 30) : 0; }
  if (vid.requestVideoFrameCallback) {
    let last = null;
    const cb = (now, md) => {
      const med = S.media[vid.dataset.mid];
      if (med && last != null && md.mediaTime > last) { const d = (md.mediaTime - last) * 1e6; if (d > 4000 && d < 250000 && vid.playbackRate <= 1) S.frameUs[med.id] = Math.min(S.frameUs[med.id] || 1e9, Math.round(d)); }
      last = md.mediaTime; vid.requestVideoFrameCallback(cb);
    };
    vid.requestVideoFrameCallback(cb);
  }
  function playPause() { if (S.mode !== "video") return; if (vid.paused) vid.play().catch((e) => ($("rv-seekinfo").textContent = "Play blocked: " + e.message)); else vid.pause(); }
  function stepFrame(dir) {
    if (S.mode !== "video") return; if (!vid.paused) { $("rv-seekinfo").textContent = "Pause first, then step by frame."; return; }
    const med = S.media[vid.dataset.mid]; seek(vNow() + dir * frameUs(med));
  }
  function nav(dir) {
    const l = S.list; if (!l.length) return; let i = S.cur ? l.findIndex((m) => m.id === S.cur.id) : -1;
    if (i < 0) i = dir > 0 ? -1 : l.length; const m = l[clamp(i + dir, 0, l.length - 1)]; if (m) loadMatch(m, { seek: m.start_us, play: !vid.paused && S.mode === "video" });
  }
  function revealInList(m) {
    const i = rows.findIndex((r) => r.m && r.m.id === m.id); if (i < 0) return; const box = $("rv-list"), y = rowY[i];
    if (y < box.scrollTop || y + 52 > box.scrollTop + box.clientHeight) box.scrollTop = Math.max(0, y - box.clientHeight / 2); paintList();
    const idx = S.list.indexOf(m); if (idx >= 0) railSeq.reveal(idx); railSeq.redraw();
  }

  // ---------- still image contact sheet
  function showSheet(m) {
    vid.pause(); vid.style.visibility = "hidden"; $("rv-msg").className = "rv-msg hide"; S.mode = "sheet"; clearOverlay();
    const sh = $("rv-sheet"), lane = m._lane; sh.classList.remove("hide");
    const src = (x) => x.thumbnail || `/api/v1/media/${encodeURIComponent(x.media_id)}/frame?t_us=${x.start_us}`;
    if (sh.dataset.lane !== lane.key) {
      sh.dataset.lane = lane.key;
      sh.innerHTML = `<div class="big"><img id="sh-img" alt=""><canvas id="sh-ov" aria-hidden="true"></canvas></div><div class="grid" id="sh-grid" role="group" aria-label="Contact sheet">${lane.matches.map((x) => `<button data-id="${esc(x.id)}" aria-label="Image ${x.si + 1}" aria-pressed="false" style="background-image:url('${esc(src(x))}')"></button>`).join("")}</div>`;
      $("sh-grid").onclick = (ev) => { const b = ev.target.closest("button"); if (b) loadMatch(S.byId[b.dataset.id]); };
    }
    $("sh-grid").querySelectorAll("button").forEach((b) => { const on = b.dataset.id === m.id; b.setAttribute("aria-pressed", on); if (on) b.scrollIntoView({ block: "nearest", inline: "center" }); });
    const img = $("sh-img"); img.alt = `Image ${m.si + 1} of ${lane.matches.length}, ${m.id}`; img.onload = () => drawStillOverlay(m); img.src = src(m);
    $("rv-hud").textContent = `image ${m.si + 1} / ${lane.matches.length} · contact sheet (no duration)`;
  }
  async function drawStillOverlay(m) {
    const img = $("sh-img"), cv = $("sh-ov"); if (!img || !cv) return;
    const r = img.getBoundingClientRect(), p = img.parentElement.getBoundingClientRect();
    cv.style.left = r.left - p.left + "px"; cv.style.top = r.top - p.top + "px"; cv.width = r.width; cv.height = r.height; cv.style.width = r.width + "px"; cv.style.height = r.height + "px";
    let obs = []; try { obs = await api("GET", `/api/v1/media/${encodeURIComponent(m.media_id)}/observations?start_us=${m.start_us}&end_us=${m.start_us + 1}`); obs = Array.isArray(obs) ? obs : (obs && obs.observations) || []; } catch (_) {}
    if (S.cur !== m) return;
    const ids = new Set(m.observation_ids); const sel = obs.filter((o) => !ids.size || ids.has(o.observation.observation_id));
    paintBoxes(cv.getContext("2d"), 0, 0, r.width, r.height, sel.flatMap((o) => o.detections || []), sel.flatMap((o) => o.relations || []), [], true);
  }

  // ---------- overlay
  const OBS = {};
  function obsChunk(mid, idx) {
    const o = OBS[mid] || (OBS[mid] = { chunks: new Map(), step: null }); let c = o.chunks.get(idx); if (c) return c;
    c = { arr: null }; o.chunks.set(idx, c);
    api("GET", `/api/v1/media/${encodeURIComponent(mid)}/observations?start_us=${idx * CH}&end_us=${(idx + 1) * CH}`).then((r) => {
      c.arr = (Array.isArray(r) ? r : (r && r.observations) || []).sort((a, b) => a.observation.media_time_us - b.observation.media_time_us);
      const d = []; for (let i = 1; i < c.arr.length; i++) { const x = c.arr[i].observation.media_time_us - c.arr[i - 1].observation.media_time_us; if (x > 0 && x <= 10e6) d.push(x); }
      if (d.length) { d.sort((a, b) => a - b); o.step = d[d.length >> 1]; } S.ovDirty = 1;
    }).catch((e) => { c.arr = []; c.err = errText(e); S.ovDirty = 1; });
    return c;
  }
  function nearestObs(mid, t) {
    const idx = Math.floor(t / CH), c = obsChunk(mid, idx), o = OBS[mid];
    if (t % CH < 2e6 && idx > 0) obsChunk(mid, idx - 1); if (t % CH > CH - 2e6) obsChunk(mid, idx + 1);
    if (c.arr == null) return { loading: true };
    if (c.err) return { err: c.err };
    let best = null, bd = Infinity;
    for (const k of [idx - 1, idx, idx + 1]) {
      const cc = o.chunks.get(k); if (!cc || !cc.arr || !cc.arr.length) continue; const a = cc.arr; let lo = 0, hi = a.length;
      while (lo < hi) { const q = (lo + hi) >> 1; if (a[q].observation.media_time_us < t) lo = q + 1; else hi = q; }
      for (const j of [lo - 1, lo]) if (a[j]) { const d = Math.abs(a[j].observation.media_time_us - t); if (d < bd) { bd = d; best = a[j]; } }
    }
    return { rec: best, dt: bd, step: o.step || 1e6 };
  }
  const AN = Watch.Annotate.create(), EMPTY = [];
  function clearOverlay() { const cv = $("rv-ov"), c = cv.getContext("2d"); c.clearRect(0, 0, cv.width, cv.height); }
  function colorFor(cls) { return PAL[cls] || "#c5cfcb"; }
  function paintBoxes(c, ox, oy, w, h, dets, rels, trails, still) {
    const byId = {}; for (const d of dets) byId[d.detection_id] = d;
    c.lineWidth = 2; c.font = '12px "IBM Plex Mono", monospace'; c.textBaseline = "top";
    if (S.ov.zones) { const z = zonesFor(vid.dataset.mid); c.setLineDash([6, 4]); for (const zz of z) { c.strokeStyle = "#7fb7b7"; c.fillStyle = "rgba(127,183,183,.12)"; c.beginPath(); zz.poly.forEach(([x, y], i) => (i ? c.lineTo(ox + x * w, oy + y * h) : c.moveTo(ox + x * w, oy + y * h))); c.closePath(); c.fill(); c.stroke(); c.fillStyle = "#7fb7b7"; c.fillText(zz.name, ox + zz.poly[0][0] * w + 4, oy + zz.poly[0][1] * h + 4); } c.setLineDash([]); }
    if (S.ov.tracks) for (const tr of trails) { c.strokeStyle = "rgba(206,125,54,.9)"; c.beginPath(); tr.forEach(([x, y], i) => (i ? c.lineTo(ox + x * w, oy + y * h) : c.moveTo(ox + x * w, oy + y * h))); c.stroke(); }
    if (S.ov.rels) for (const r of rels) {
      const a = byId[r.subject_detection_id], b = byId[r.object_detection_id]; if (!a || !b) continue;
      const ca = [(a.bbox[0] + a.bbox[2]) / 2, (a.bbox[1] + a.bbox[3]) / 2], cb = [(b.bbox[0] + b.bbox[2]) / 2, (b.bbox[1] + b.bbox[3]) / 2];
      c.strokeStyle = "#CE7D36"; c.setLineDash([4, 3]); c.beginPath(); c.moveTo(ox + ca[0] * w, oy + ca[1] * h); c.lineTo(ox + cb[0] * w, oy + cb[1] * h); c.stroke(); c.setLineDash([]);
      c.fillStyle = "#CE7D36"; c.fillText(`${r.predicate}${S.ov.conf ? " " + r.confidence.toFixed(2) : ""} (approx.)`, ox + (ca[0] + cb[0]) / 2 * w, oy + (ca[1] + cb[1]) / 2 * h);
    }
    if (still && (S.ov.boxes || S.ov.labels)) for (const d of dets) {
      const [x0, y0, x1, y1] = d.bbox, col = colorFor(d.class), x = ox + x0 * w, y = oy + y0 * h;
      if (S.ov.boxes) { c.strokeStyle = col; c.strokeRect(x, y, (x1 - x0) * w, (y1 - y0) * h); }
      if (S.ov.labels || S.ov.conf) {
        const text = [S.ov.labels ? d.class : "", S.ov.conf ? d.confidence.toFixed(2) : ""].filter(Boolean).join(" ") + (S.ov.tracks && d.track_id ? " " + d.track_id : "");
        if (text) { const tw = c.measureText(text).width + 6; c.fillStyle = "rgba(10,10,10,.8)"; c.fillRect(x, Math.max(0, y - 15), tw, 15); c.fillStyle = col; c.fillText(text, x + 3, Math.max(0, y - 14)); }
      }
    }
  }
  const zonesFor = (mid) => { const m = S.media[mid]; return (m && (m.zones || m.fixture_zones)) || []; };
  let ovKey = "";
  function drawOverlay(t) {
    const cv = $("rv-ov"), box = $("rv-screen"), d = window.devicePixelRatio || 1, W = box.clientWidth, H = box.clientHeight, mid = vid.dataset.mid;
    if (cv.width !== Math.round(W * d) || cv.height !== Math.round(H * d)) { cv.width = Math.round(W * d); cv.height = Math.round(H * d); ovKey = ""; }
    const now = performance.now(), key = [Math.round(t / 1000), JSON.stringify(S.ov), W, H, S.ovDirty, AN.busy ? Math.floor(now / 16) : 0].join("|"); if (key === ovKey) return; ovKey = key; S.ovDirty = 0;
    const c = cv.getContext("2d"); c.setTransform(d, 0, 0, d, 0, 0); c.clearRect(0, 0, W, H);
    const vw = vid.videoWidth || 16, vh = vid.videoHeight || 9, sc = Math.min(W / vw, H / vh), w = vw * sc, h = vh * sc, ox = (W - w) / 2, oy = (H - h) / 2;
    let dets = EMPTY;
    const anyOv = S.ov.boxes || S.ov.labels || S.ov.rels || S.ov.tracks || S.ov.zones; let hud = "";
    if (S.ov.zones && !S.ov.boxes && !S.ov.labels && !S.ov.tracks) paintBoxes(c, ox, oy, w, h, [], [], []);
    if (anyOv && mid) {
      const r = nearestObs(mid, t);
      if (r.loading) hud = "overlay: loading observations";
      else if (r.err) hud = "overlay unavailable: " + r.err;
      else if (!r.rec) hud = "overlay: no observations here (cleared)";
      else if (r.dt > r.step / 2) hud = `overlay cleared: nearest sample is ${(r.dt / 1000).toFixed(0)} ms away, limit ${(r.step / 2000).toFixed(0)} ms (half the ${(r.step / 1000).toFixed(0)} ms sample step)`;
      else {
        const trails = []; if (S.ov.tracks) {
          const o = OBS[mid], tr = {}; for (const k of [Math.floor(t / CH) - 1, Math.floor(t / CH)]) { const cc = o.chunks.get(k); if (cc && cc.arr) for (const x of cc.arr) { const tt = x.observation.media_time_us; if (tt <= r.rec.observation.media_time_us && tt >= t - 4e6) for (const dd of x.detections) if (dd.track_id) (tr[dd.track_id] = tr[dd.track_id] || []).push([(dd.bbox[0] + dd.bbox[2]) / 2, dd.bbox[3]]); } }
          for (const k in tr) trails.push(tr[k]);
        }
        dets = r.rec.detections || EMPTY;
        paintBoxes(c, ox, oy, w, h, dets, r.rec.relations || [], trails);
        hud = `overlay: sample @ ${fmtT(r.rec.observation.media_time_us, 1)} (Δ ${(r.dt / 1000).toFixed(0)} ms)`;
      }
    }
    AN.update(S.ov.boxes || S.ov.labels ? dets : EMPTY, now);
    AN.paint(c, { x: ox, y: oy, w, h }, now, { boxes: S.ov.boxes, labels: S.ov.labels, conf: S.ov.conf, badge: S.ov.boxes });
    $("rv-hud").textContent = hud;
  }
  function loop() {
    requestAnimationFrame(loop);
    if (S.mode !== "video") return;
    const t = vNow(), v = vid, med = S.media[v.dataset.mid];
    if (S.loop && !v.paused && S.cur) { const it = getItem(S.cur), e = S.edit[S.cur.id], a = it ? it.in_us : e ? e.cs : S.cur.context_start_us, b = it ? it.out_us : e ? e.ce : S.cur.context_end_us; if (t > b) seek(a); }
    const txt = fmtT(t, 1) + (med && med.duration_us ? " / " + fmtT(Math.min(med.duration_us, med.fixture_wrap_us ? med.duration_us : med.duration_us)) : "");
    if ($("rv-clock").textContent !== txt) $("rv-clock").textContent = txt;
    if (Math.abs(t - (S._lt ?? -1e12)) > 900 || S._ll !== S.curLane) { S._lt = t; S._ll = S.curLane; tl.setPlayhead(S.curLane, t); }
    drawOverlay(t);
  }

  // ---------- top bar
  function bar() {
    const r = S.run, c = r.coverage || {}, dec = c.decoded || 0, done = (c.inferred || 0) + (c.skipped || 0), pend = Math.max(0, dec - done - (c.failed || 0));
    $("rv-sources").innerHTML = (r.scope.source_ids.length ? r.scope.source_ids : [...new Set(S.lanes.map((l) => l.source_id))]).map((s) => `<span class="rv-chip">${esc(s)}</span>`).join("") || '<span class="rv-chip">all sources</span>';
    const f = r.scope.from_utc_us, t = r.scope.to_utc_us;
    $("rv-scope").textContent = (f || t ? `${f ? utcStr(f).slice(0, 16) : "start"} → ${t ? utcStr(t).slice(0, 16) : "now"} UTC` : "all time") + (r.scope.media_ids.length ? ` · ${r.scope.media_ids.length} media` : "");
    queryPane(r);
    const mod = S.models ? `${S.models.length} module${S.models.length === 1 ? "" : "s"}` : FIX ? "2 modules" : "n/a";
    $("rv-stats").innerHTML = [["Results", `${S.matches.length.toLocaleString()} matches` + (S.matches.length !== r.result_count ? ` (${r.result_count.toLocaleString()} ${r.kind})` : "")],
      ["Coverage", dec ? `${Math.round(done / dec * 100)}% analyzed` : "no frames", `decoded ${dec} · inferred ${c.inferred || 0} · skipped ${c.skipped || 0} · failed ${c.failed || 0}`],
      ["Models", mod + (r.warnings.length ? ` · ${r.warnings.length} warning${r.warnings.length > 1 ? "s" : ""}` : "")], ["Pending", pend ? pend.toLocaleString() + " frames" : "none", "decoded minus inferred, skipped, failed", pend ? "warn-t" : ""],
      ["Duration", (r.elapsed_us / 1000).toFixed(1) + " ms"], ["Snapshot", "#" + r.snapshot]].map(([k, v, tt, cl]) => `<div title="${esc(tt || "")}"><dt>${k}</dt><dd class="${cl || ""}">${esc(v)}</dd></div>`).join("");
    const w = $("rv-warn"); w.textContent = r.warnings.join(" · "); w.classList.toggle("hide", !r.warnings.length);
    const sel = $("fl-src"); sel.innerHTML = '<option value="">all cameras</option>' + [...new Set(S.lanes.map((l) => l.source_id))].map((s) => `<option>${esc(s)}</option>`).join("");
  }
  function state(kind, title, text, link) {
    const el = $("rv-state"); el.className = "rv-state" + (kind === "err" ? " err" : ""); el.innerHTML = `<b>${esc(title)}</b><span>${esc(text)}</span>${link || ""}`;
    $("rv-work").classList.add("hide"); document.querySelector(".rv-edit").classList.add("hide");
  }

  // ---------- boot
  async function boot() {
    Watch.start(null, 3000);
    $("rv-state").className = "rv-state"; $("rv-state").innerHTML = "<b>Loading review…</b>"; $("rv-work").classList.add("hide");
    try {
      let tlResp, extra = [];
      if (FIX) {
        $("rv-test").classList.remove("hide"); document.title = "TEST DATA · Watch · Review";
        S.fx = await (await fetch("/ui/fixtures/review.json")).json(); S.run = S.fx.run;
        const g = genFixture(S.fx); tlResp = g; S.run.result_count = g.lanes.reduce((a, l) => a + l.matches.length, 0);
      } else {
        let id = qs.get("run");
        if (!id && (qs.get("source") || qs.get("media"))) {
          const sc = { source_ids: qs.get("source") ? [qs.get("source")] : [], media_ids: qs.get("media") ? [qs.get("media")] : [], from_utc_us: null, to_utc_us: null };
          try { id = (await api("POST", "/api/v1/queries", { sql: "SELECT * FROM detections", scope: sc })).run.id; const u = new URL(location.href); u.searchParams.set("run", id); history.replaceState(null, "", u); } catch (e) { throw e; }
        }
        if (!id) { state("empty", "No query run selected", "Open a review from a search result, or use /review?run=<query run id>.", '<p><a href="/find">Go to Search</a></p>'); return; }
        { const rr = await api("GET", "/api/v1/queries/" + encodeURIComponent(id)); S.run = rr.run || rr; }
        const [t, first] = await Promise.all([api("GET", `/api/v1/queries/${encodeURIComponent(id)}/timeline`), api("GET", `/api/v1/queries/${encodeURIComponent(id)}/matches?limit=500`).catch((e) => ({ err: e }))]);
        tlResp = t; if (first.err) { if (!(t.lanes || []).some((l) => (l.matches || []).length)) throw first.err; } else { extra = Array.isArray(first) ? first : first.matches || []; S.nextCursor = first.next || null; }
        api("GET", "/api/v1/models").then((m) => { S.models = Array.isArray(m) ? m : m.models || []; bar(); }).catch(() => {});
      }
      const ids = [...new Set((tlResp.lanes || []).map((l) => l.media_id).concat(extra.map((m) => m.media_id)).filter(Boolean))];
      if (!FIX) await Promise.allSettled(ids.map(async (i) => { try { S.media[i] = await api("GET", "/api/v1/media/" + encodeURIComponent(i)); } catch (_) {} }));
      S.lanes = buildLanes(tlResp, extra); indexMatches();
      if (!S.matches.length && !S.lanes.length) { bar(); state("empty", "No results", `This query matched nothing. ${S.run.coverage.decoded ? "Frames analyzed: " + (S.run.coverage.inferred + S.run.coverage.skipped) + " of " + S.run.coverage.decoded + "." : "No frames have been indexed for this scope."} Widen the scope, lower the confidence, or wait for pending scans.`, '<p><a href="/find">Back to Search</a></p>'); return; }
      $("rv-state").className = "rv-state hide"; $("rv-work").classList.remove("hide");
      tl.setLanes(S.lanes); bar(); applyFilter(); tl.fitAll(); refreshSel();
      const rid = qs.get("review");
      if (rid) { try { applyReview(await api("GET", "/api/v1/reviews/" + encodeURIComponent(rid))); refreshSel(); } catch (e) { S.saveMsg = "Could not load review " + rid + ". " + errText(e); renderDetails(); } }
      const win = qs.get("start_us") != null && qs.get("media") ? { a: +qs.get("start_us"), b: qs.get("end_us") != null ? +qs.get("end_us") : +qs.get("start_us") } : null;
      const inWin = win && S.matches.find((m) => m.media_id === qs.get("media") && m.end_us >= win.a && m.start_us <= win.b);
      const want = inWin || (qs.get("match") && S.byId[qs.get("match")]) || S.list.find((m) => !m.still && m._lane.availability === "available") || S.list[0];
      if (want) { await loadMatch(want, { seek: inWin ? clamp(win.a, want.start_us, want.end_us) : want.start_us }); fitSel(); }
      else if (win && S.laneOf[qs.get("media")]) { seekLane(S.laneOf[qs.get("media")].idx, win.a); }
      if (!FIX && S.nextCursor) pageMore();
      requestAnimationFrame(loop);
    } catch (e) { state("err", "Could not load this review", errText(e), '<p><a href="">Retry</a></p>'); }
  }
  async function pageMore() {
    let cur = S.nextCursor, n = 0;
    while (cur && n++ < 200) {
      try { const r = await api("GET", `/api/v1/queries/${encodeURIComponent(S.run.id)}/matches?limit=500&cursor=${encodeURIComponent(cur)}`); const arr = Array.isArray(r) ? r : r.matches || []; cur = r.next || null;
        const fresh = arr.filter((m) => !S.byId[m.id]); if (fresh.length) { const keep = S.cur; const lanes = buildLanes({ lanes: S.lanes.map((l) => ({ source_id: l.source_id, media_id: l.media_id, label: l.label, clock: { kind: l.clock }, duration_us: l.dur, availability: l.availability, recorded: l.recorded, recording: l.recording, capture_start_utc_us: l.cap, matches: l.matches })), gaps: S.lanes.flatMap((l) => l.gaps) }, fresh); S.lanes = lanes; indexMatches(); tl.setLanes(S.lanes); applyFilter(); bar(); if (keep) { S.curLane = keep._lane.idx; tl.setCurrent(keep, S.curLane); } }
      } catch (e) { $("rv-warn").textContent = "Paging stopped: " + errText(e); $("rv-warn").classList.remove("hide"); break; }
    }
  }

  // ---------- query pane (read-only, highlighted)
  const KW = new Set("select from where and or not in like limit order by group having join on as count distinct between is null desc asc sum avg min max over interval within near true false function return end if then else elseif for while do local nil".split(" "));
  function hl(src) {
    return src.replace(/('[^']*'|"[^"]*"|--[^\n]*|\b\d+(?:\.\d+)?\b|\b[A-Za-z_][A-Za-z0-9_]*\b)(\s*\()?|[\s\S]/g, (t, tok, call) => {
      if (!tok) return esc(t);
      const cls = tok[0] === "'" || tok[0] === '"' ? "s" : tok.startsWith("--") ? "c" : /^\d/.test(tok) ? "n" : KW.has(tok.toLowerCase()) ? "k" : call ? "f" : "";
      return (cls ? `<span class="${cls}">${esc(tok)}</span>` : esc(tok)) + esc(call || "");
    });
  }
  function queryPane(r) {
    const lua = /\bfunction\b/.test(r.normalized) && !/^\s*select/i.test(r.normalized), c = r.coverage || {};
    $("rv-qpane").innerHTML = `<div class="quiet">${lua ? "Lua predicate" : "SQL"} (read-only here)</div><pre class="rv-sql" tabindex="0" aria-label="Query text">${hl(r.normalized)}</pre>
      <div class="row"><a href="/find" class="mono">Edit in Search</a></div>
      <dl><dt>run</dt><dd>${esc(r.id)}</dd><dt>kind</dt><dd>${esc(r.kind)}</dd><dt>snapshot</dt><dd>#${r.snapshot}</dd><dt>schema</dt><dd>v${r.schema_version}</dd><dt>elapsed</dt><dd>${(r.elapsed_us / 1000).toFixed(1)} ms</dd>
      <dt>sources</dt><dd>${esc(r.scope.source_ids.join(", ") || "all")}</dd><dt>media</dt><dd>${esc(r.scope.media_ids.join(", ") || "all")}</dd>
      <dt>from</dt><dd>${r.scope.from_utc_us ? esc(utcStr(r.scope.from_utc_us)) : "start"}</dd><dt>to</dt><dd>${r.scope.to_utc_us ? esc(utcStr(r.scope.to_utc_us)) : "now"}</dd>
      <dt>decoded</dt><dd>${c.decoded || 0}</dd><dt>inferred</dt><dd>${c.inferred || 0}</dd><dt>skipped</dt><dd>${c.skipped || 0}</dd><dt>failed</dt><dd>${c.failed || 0}</dd></dl>
      ${r.warnings.length ? `<div class="rv-callout">${r.warnings.map(esc).join("<br>")}</div>` : ""}`;
  }

  // ---------- panel layout: splitters, collapse, tabs (state kept in localStorage when available)
  const rv = $("rv"), work = $("rv-work");
  const lsGet = (k) => { try { return localStorage.getItem("rv." + k); } catch (_) { return null; } }, lsSet = (k, v) => { try { localStorage.setItem("rv." + k, v); } catch (_) {} };
  const setVar = (n, v) => { rv.style.setProperty(n, v); };
  for (const [k, v] of [["wl", "--wl"], ["wr", "--wr"], ["hed", "--hed"]]) { const x = lsGet(k); if (x) setVar(v, x); }
  function fitHdr() { const h = document.querySelector("header"), c = $("connection"); rv.style.setProperty("--hdr", ((h ? h.offsetHeight : 56) + (c && !c.classList.contains("hide") ? c.offsetHeight : 0)) + "px"); }
  fitHdr(); window.addEventListener("resize", fitHdr);
  function collapse(side, on) { work.classList.toggle(side === "left" ? "nl" : "nr", on); $(side === "left" ? "p-left" : "p-right").setAttribute("aria-pressed", !on); lsSet("c" + side, on ? "1" : ""); tl.resize(); railSeq.resize(); railSel.resize(); }
  document.querySelectorAll("[data-collapse]").forEach((b) => (b.onclick = () => collapse(b.dataset.collapse, true)));
  $("p-left").onclick = () => collapse("left", !work.classList.contains("nl")); $("p-right").onclick = () => collapse("right", !work.classList.contains("nr"));
  if (lsGet("cleft")) collapse("left", true); if (lsGet("cright")) collapse("right", true);
  document.querySelectorAll(".rv-split").forEach((sp) => {
    const kind = sp.dataset.split, cfg = { left: ["--wl", "wl", 140, 520, 1], right: ["--wr", "wr", 180, 640, -1], edit: ["--hed", "hed", 120, 0, -1] }[kind];
    const cur = () => parseFloat(getComputedStyle(rv).getPropertyValue(cfg[0])) || (kind === "edit" ? $("rv-tl").parentElement.offsetHeight : 250);
    const apply = (px) => { const max = kind === "edit" ? Math.max(200, rv.clientHeight - 260) : cfg[3]; px = clamp(px, cfg[2], max); setVar(cfg[0], px + "px"); lsSet(cfg[1], px + "px"); tl.resize(); railSeq.resize(); railSel.resize(); };
    sp.addEventListener("pointerdown", (e) => { sp.setPointerCapture(e.pointerId); sp.classList.add("on"); const x0 = kind === "edit" ? e.clientY : e.clientX, v0 = kind === "edit" ? $("rv-tl").closest(".rv-edit").offsetHeight : cur();
      const mv = (ev) => apply(v0 + ((kind === "edit" ? ev.clientY : ev.clientX) - x0) * cfg[4]); const up = () => { sp.classList.remove("on"); sp.removeEventListener("pointermove", mv); sp.removeEventListener("pointerup", up); }; sp.addEventListener("pointermove", mv); sp.addEventListener("pointerup", up); });
    sp.addEventListener("keydown", (e) => { const d = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[e.key]; if (!d) return; e.preventDefault(); e.stopPropagation(); apply((kind === "edit" ? $("rv-tl").closest(".rv-edit").offsetHeight : cur()) + d * 24 * cfg[4]); });
  });
  function setSub(n) { $("rv-right").dataset.sub = n; document.querySelectorAll(".rv-subtabs [data-sub]").forEach((b) => b.setAttribute("aria-pressed", b.dataset.sub === n)); }
  document.querySelectorAll(".rv-subtabs [data-sub]").forEach((b) => (b.onclick = () => setSub(b.dataset.sub)));
  document.querySelectorAll("#rv-mtabs [data-tab]").forEach((b) => (b.onclick = () => { rv.dataset.tab = b.dataset.tab; if (b.dataset.tab === "query") setSub("query"); else if (b.dataset.tab === "details") setSub("details"); document.querySelectorAll("#rv-mtabs button").forEach((x) => x.setAttribute("aria-pressed", x === b)); setTimeout(() => { tl.resize(); railSeq.resize(); railSel.resize(); }, 0); }));

  // ---------- wiring
  $("rv-list").addEventListener("scroll", paintList, { passive: true });
  $("rv-list").addEventListener("click", (e) => { const r = e.target.closest(".rv-row[data-id]"); if (r) loadMatch(S.byId[r.dataset.id], { seek: S.byId[r.dataset.id].start_us, user: true }); });
  $("rv-list").addEventListener("keydown", (e) => { if (e.key === "ArrowDown" || e.key === "ArrowUp") { nav(e.key === "ArrowDown" ? 1 : -1); e.preventDefault(); e.stopPropagation(); } });
  const ff = (id, k, fn) => $(id).addEventListener(k, () => { fn($(id)); applyFilter(); });
  ff("fl-class", "input", (n) => (S.filter.text = n.value)); ff("fl-ver", "change", (n) => (S.filter.ver = n.value)); ff("fl-src", "change", (n) => (S.filter.src = n.value));
  ff("fl-conf", "input", (n) => { S.filter.conf = n.value / 100; $("fl-conf-o").textContent = (n.value / 100).toFixed(2); }); ff("fl-group", "change", (n) => (S.filter.group = n.checked)); ff("fl-sort", "change", (n) => (S.filter.sort = n.value));
  $("b-play").onclick = playPause; $("b-prev").onclick = () => nav(-1); $("b-next").onclick = () => nav(1); $("b-fb").onclick = () => stepFrame(-1); $("b-ff").onclick = () => stepFrame(1);
  $("b-rate").onchange = () => (vid.playbackRate = +$("b-rate").value);
  const toggleBtn = (id, f) => { const b = $(id); b.onclick = () => { const on = b.getAttribute("aria-pressed") !== "true"; b.setAttribute("aria-pressed", on); f(on); }; };
  toggleBtn("b-mute", (on) => (vid.muted = on)); toggleBtn("b-loop", (on) => (S.loop = on));
  document.querySelectorAll("[data-ov]").forEach((b) => (b.onclick = () => ovToggle(b.dataset.ov)));
  function ovToggle(k) { S.ov[k] = S.ov[k] ? 0 : 1; const b = document.querySelector(`[data-ov="${k}"]`); b.setAttribute("aria-pressed", !!S.ov[k]); S.ovDirty = 1; if (S.cur && S.cur.still) drawStillOverlay(S.cur); }
  function setView(v) { S.view = v; tl.setMode(v); fitSel(); document.querySelectorAll("[data-view]").forEach((b) => b.setAttribute("aria-pressed", b.dataset.view === v)); }
  document.querySelectorAll("[data-view]").forEach((b) => (b.onclick = () => setView(b.dataset.view)));
  $("z-in").onclick = () => tl.zoomBy(1.5); $("z-out").onclick = () => tl.zoomBy(1 / 1.5); $("z-all").onclick = () => tl.fitAll();
  function fitSel() { const m = S.cur; if (!m || m.still) return; const it = getItem(m), e = S.edit[m.id]; tl.fitRange(S.curLane, m, it ? it.in_us : e ? e.cs : m.context_start_us, it ? it.out_us : e ? e.ce : m.context_end_us); }
  $("z-sel").onclick = fitSel;
  $("t-follow").onchange = (e) => (tl.follow = e.target.checked);
  $("t-align").onchange = (e) => { const lanes = S.lanes.filter((l) => l.clock === "utc" && l.cap != null); const mn = Math.min(...lanes.map((l) => l.cap)); for (const l of S.lanes) l.off = e.target.checked && l.clock === "utc" && l.cap != null ? l.cap - mn : 0; tl.layout(); tl.fitAll(); };
  const KEYS = [["Space", "Play / pause"], ["J or [", "Previous match"], ["K or ]", "Next match"], [", and .", "Step one frame back / forward (paused)"], ["Left / Right", "Seek 1 s (Shift: 5 s)"], ["+ and -", "Zoom timeline (Ctrl+wheel also)"], ["F / Shift+F", "Fit selection / fit all"], ["O", "Toggle original-time and matched-only view"], ["M / L", "Mute / loop trim range"], ["< and >", "Slower / faster playback"], ["B, Shift+L, C, R, T, Z", "Overlays: boxes, labels, confidence, relations, tracks, zones"], ["A", "Add / remove current match in selection"], ["V / X / E / U", "Confirm / reject / reviewed / unreviewed"], ["Ctrl+S", "Save selection"], ["Alt+Left/Right", "Reorder focused selected clip"], ["?", "This help"]];
  $("rv-keymap").innerHTML = `<table>${KEYS.map(([k, d]) => `<tr><td><kbd>${esc(k)}</kbd></td><td>${esc(d)}</td></tr>`).join("")}</table>`;
  $("b-help").onclick = () => $("rv-keys").showModal();
  document.addEventListener("keyup", (e) => { if (e.key === " " && !/INPUT|TEXTAREA|SELECT|SUMMARY/.test(e.target.tagName) && !$("rv-keys").open) e.preventDefault(); });
  document.addEventListener("keydown", (e) => {
    const tg = e.target.tagName; if (/INPUT|TEXTAREA|SELECT/.test(tg) || e.target.isContentEditable) return;
    if ($("rv-keys").open) return;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") { e.preventDefault(); saveSel(); return; }
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const k = e.key; let used = true;
    if (k === " ") { if (tg === "SUMMARY") return; playPause(); }
    else if (k === "j" || k === "[") nav(-1); else if (k === "k" || k === "]") nav(1);
    else if (k === ",") stepFrame(-1); else if (k === ".") stepFrame(1);
    else if (k === "ArrowLeft" || k === "ArrowRight") { if (e.target.closest(".rail") || e.target.closest("#rv-list")) return; if (S.mode === "video") seek(vNow() + (k === "ArrowRight" ? 1 : -1) * (e.shiftKey ? 5e6 : 1e6)); }
    else if (k === "+" || k === "=") tl.zoomBy(1.5); else if (k === "-" || k === "_") tl.zoomBy(1 / 1.5);
    else if (k === "f") fitSel(); else if (k === "F") tl.fitAll(); else if (k === "o") setView(S.view === "orig" ? "matched" : "orig");
    else if (k === "m") $("b-mute").click(); else if (k === "l") $("b-loop").click();
    else if (k === "<" || k === ">") { const sel = $("b-rate"); sel.selectedIndex = clamp(sel.selectedIndex + (k === ">" ? 1 : -1), 0, sel.options.length - 1); sel.onchange(); }
    else if (k === "b") ovToggle("boxes"); else if (k === "L") ovToggle("labels"); else if (k === "c") ovToggle("conf"); else if (k === "r") ovToggle("rels"); else if (k === "t") ovToggle("tracks"); else if (k === "z") ovToggle("zones");
    else if (k === "a") toggleItem(S.cur); else if (k === "v") setVerify("confirmed"); else if (k === "x") setVerify("rejected"); else if (k === "e") setVerify("reviewed"); else if (k === "u") setVerify("unreviewed");
    else if (k === "?") $("rv-keys").showModal(); else used = false;
    if (used) e.preventDefault();
  });
  // honest perf readout: draw cost and long tasks
  let longTasks = 0; try { new PerformanceObserver((l) => { longTasks += l.getEntries().length; }).observe({ entryTypes: ["longtask"] }); } catch (_) {}
  setInterval(() => { $("rv-perf").textContent = `drawing ${tl.drawn || 0} of ${S.matches.length.toLocaleString()} strips · last draw ${(tl.ms || 0).toFixed(1)} ms · long tasks ${longTasks}`; }, 500);
  window.addEventListener("beforeunload", (e) => { if (S.dirty) { e.preventDefault(); e.returnValue = ""; } });
  boot();
})();
