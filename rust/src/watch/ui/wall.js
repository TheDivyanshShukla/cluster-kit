/* Product API client. /api/v1 first; a 404 with no ApiError envelope means "route not on this server", so the page falls back to the legacy endpoint (or says so). */
const V = (() => {
  const esc = Watch.esc;
  async function call(method, url, body) {
    let r;
    try { r = await fetch(url, { method, headers: body === undefined ? {} : { "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) }); }
    catch (_) { throw { code: "network", message: "Cannot reach the server.", retryable: true, status: 0 }; }
    const text = await r.text();
    let j = null; try { j = text ? JSON.parse(text) : null; } catch (_) {}
    if (r.ok) return j;
    const e = j && j.error, env = !!(e && typeof e === "object");
    throw { code: env ? e.code : "http_" + r.status, message: env ? e.message : (typeof e === "string" ? e : "Request failed (" + r.status + ")"), retryable: env ? !!e.retryable : r.status >= 500, status: r.status, envelope: env, correlation_id: env ? e.correlation_id : undefined };
  }
  const absent = (e) => !!e && e.status === 404 && !e.envelope;
  async function pick(v1, legacy) {
    try { return { v: 1, data: await v1() }; }
    catch (e) {
      if (!absent(e)) throw e;
      if (!legacy) return { v: 0, unavailable: true };
      return { v: 0, data: await legacy() };
    }
  }
  const list = (j, key) => Array.isArray(j) ? j : (j && (Array.isArray(j[key]) ? j[key] : Array.isArray(j.items) ? j.items : [])) || [];
  // /api/v1/models entries are {model: ModelVersion|null, module, status}; accept bare ModelVersions too.
  const modelsOf = (j) => list(j, "models").map((e) => (e && e.model) || (e && e.module && e.model === undefined ? null : e)).filter(Boolean);
  // Show an error (message, correlation id, Retry when the server says retryable) in element `el`.
  function show(el, e, retry) {
    if (!el) return;
    el.className = "msg bad"; el.setAttribute("role", "alert");
    const m = (e && e.message) || String(e);
    el.innerHTML = esc(m) + (e && e.code && e.envelope ? ` <span class="quiet">(${esc(e.code)}${e.correlation_id ? ", " + esc(e.correlation_id) : ""})</span>` : "") + (e && e.retryable && retry ? ' <button type="button" data-retry>Retry</button>' : "");
    const b = el.querySelector("[data-retry]"); if (b) b.onclick = () => { el.textContent = ""; retry(); };
  }
  function ok(el, text) { if (!el) return; el.className = "msg ok"; el.setAttribute("role", "status"); el.textContent = text; }
  function clear(el) { if (el) { el.className = "msg"; el.textContent = ""; } }
  // Sign-in state. 401 -> sign-in required; 404 -> no login configured.
  async function auth() {
    let bar = document.getElementById("auth-state");
    if (!bar) { bar = document.createElement("div"); bar.id = "auth-state"; const c = document.getElementById("connection"); c.parentNode.insertBefore(bar, c.nextSibling); }
    try {
      const me = await call("GET", "/api/v1/auth/me");
      const pr = me && me.principal ? me.principal : me, who = pr && (pr.name || pr.actor || pr.user || pr.username);
      bar.className = "auth-bar"; bar.textContent = "Signed in" + (who ? " as " + (typeof who === "object" ? (who.name || who.id || "") : who) : "") + (pr && pr.role ? " · " + pr.role : "") + (pr && pr.mode ? " (" + pr.mode + ")" : "") + ".";
    } catch (e) {
      if (e.status === 401) { bar.className = "auth-bar need"; bar.innerHTML = 'Sign-in required. Actions will be refused until you <a href="/login?next=' + encodeURIComponent(location.pathname + location.search) + '">sign in</a>.'; }
      else if (absent(e)) { bar.className = "auth-bar"; bar.textContent = "No login configured — trusted LAN only."; }
      else { bar.className = "auth-bar"; bar.textContent = "Sign-in state unavailable: " + e.message; }
    }
  }
  const usTime = (us) => { const s = Math.max(0, Number(us) || 0) / 1e6, m = Math.floor(s / 60); return m + ":" + (s - m * 60).toFixed(1).padStart(4, "0"); };
  const utcTime = (us) => us == null ? "" : new Date(Number(us) / 1000).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const analyzed = (c) => ((c && c.analyzed) || []).reduce((a, r) => a + Math.max(0, r[1] - r[0]), 0);
  const redact = (s) => String(s || "").replace(/(\/\/)[^\/@\s]*@/, "$1***@");
  const n = (x) => Number(x || 0).toLocaleString();
  return { call, absent, pick, list, modelsOf, show, ok, clear, auth, usTime, utcTime, analyzed, redact, n, esc };
})();
const $ = Watch.$, esc = Watch.esc;
let alerts = [], alertApi = null, alertKey = "", extra = [], extraAll = [], extraKey = "", cams = [];
let layout = "auto", solo = "", page = 0, seen = null, flash = {};
// "boxes": the real recording in a muted looping <video>, boxes drawn on a canvas synced to video.currentTime.
// "sync": draw each analyzed frame together with its own detections (boxes always match the picture).
// "live": the head's MJPEG preview of a really running source; decoded separately, so no boxes.
let view = "boxes";
try { const v = localStorage.getItem("watch.wallView2"); if (v === "boxes" || v === "sync" || v === "live") view = v; } catch (_) {}
try { layout = localStorage.getItem("watch.wallLayout") || "auto"; } catch (_) {}

function slotCount(n) {
  if (solo) return { cols: 1, rows: 1, slots: 1 };
  const k = layout === "auto" ? 0 : Number(layout);
  if (k === 1) return { cols: 1, rows: 1, slots: 1 };
  if (k) return { cols: k, rows: k, slots: k * k };
  const c = Math.max(1, Math.ceil(Math.sqrt(Math.max(n, 1)))), r = Math.max(1, Math.ceil(n / c));
  return { cols: c, rows: r, slots: c * r };
}
const isRep = (id) => !!repOf[id];
function tileHtml(c) {
  return `<div class="ct" role="listitem" tabindex="0" data-cam="${esc(c.id)}" aria-label="${esc(c.name)}: click to solo, double-click for fullscreen">
    ${view === "live" && !isRep(c.id) ? `<img alt="" src="/api/live/${encodeURIComponent(c.id)}">` : `<canvas class="base" aria-hidden="true"></canvas>`}
    ${view === "boxes" || (view === "live" && isRep(c.id)) ? `<video class="vid" muted loop playsinline autoplay preload="auto" aria-hidden="true"></video><canvas class="ov" aria-hidden="true"></canvas>` : ""}
    <div class="ct-sig">Waiting for first frame…</div><div class="ct-note" hidden></div>
    <div class="ct-top"><span class="dot"></span><b class="ct-name"></b><span class="ct-tag" hidden title="This camera replays a recording on a loop, like a live feed. The position follows the wall clock, so every viewer sees the same moment. Boxes are the stored detections for that moment.">LIVE · replayed from recording</span><span class="ct-fps mono"></span></div>
    <div class="ct-bot"><span class="ct-people mono"></span><span class="ct-stat mono"></span><a class="ct-rev" href="/clip?id=${encodeURIComponent(c.id)}">Review</a></div></div>`;
}
const noSignal = (label) => `<div class="ct empty" role="listitem"><div class="ct-sig" style="display:flex">No signal${label ? " · " + esc(label) : ""}</div></div>`;
function wire(el) {
  const img = el.querySelector("img"), n = el.querySelector(".ct-sig"), dot = el.querySelector(".dot");
  if (img) {
    img.onload = () => { n.style.display = "none"; el.dataset.sig = "1"; };
    img.onerror = () => { img.classList.add("hide"); n.textContent = "No signal — this source is not producing frames."; n.style.display = "flex"; dot.className = "dot bad"; el.dataset.sig = "0"; };
  }
  el.onclick = (e) => { if (e.target.closest("a")) return; setSolo(solo === el.dataset.cam ? "" : el.dataset.cam); };
  el.ondblclick = (e) => { if (!e.target.closest("a")) fullscreen(el); };
  el.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); el.click(); } };
}
function fullscreen(el) { const t = el || $("cctv"); if (document.fullscreenElement) document.exitFullscreen(); else if (t.requestFullscreen) t.requestFullscreen().catch(() => {}); }
function setSolo(id) { solo = id; render(true); }
function setLayout(l) {
  layout = l; solo = ""; page = 0;
  try { localStorage.setItem("watch.wallLayout", l); } catch (_) {}
  document.querySelectorAll("#layouts button").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.l === l)));
  render(true);
}
function render(force) {
  const have = new Set(cams.map((c) => c.id)); extra = extraAll.filter((x) => !have.has(x.id));
  const all = cams.map((c) => ({ c })).concat(extra.map((x) => ({ x })));
  let list = solo ? all.filter((e) => e.c && e.c.id === solo) : all;
  if (solo && !list.length) { solo = ""; list = all; }
  const g = slotCount(list.length), per = g.slots, pages = Math.max(1, Math.ceil(list.length / per));
  page = Math.min(page, pages - 1);
  const shown = list.slice(page * per, page * per + per);
  const key = [layout, solo, page, shown.map((e) => (e.c ? e.c.id : "x:" + e.x.id)).join(","), g.cols, g.rows].join("|");
  const tiles = $("tiles");
  $("pager").hidden = pages < 2; $("pg-label").textContent = (page + 1) + "/" + pages;
  if (force || tiles.dataset.key !== key) {
    tiles.dataset.key = key; teardown();
    tiles.style.setProperty("--cols", g.cols); tiles.style.setProperty("--rows", g.rows);
    tiles.innerHTML = shown.map((e) => e.c ? tileHtml(e.c) : noSignal((e.x.name || e.x.id) + (e.x.enabled === false ? " (disabled)" : " (no worker reports it)"))).join("") + Array(Math.max(0, per - shown.length)).fill(noSignal("empty slot")).join("");
    tiles.querySelectorAll(".ct[data-cam]").forEach((el) => {
      wire(el); ro.observe(el);
      const f = frames[el.dataset.cam]; if (f) draw(el, f); // a rebuilt tile shows its last analysed picture at once
      if (view === "boxes" || isRep(el.dataset.cam)) { io.observe(el); setupVideo(el); }
    });
  }
  tiles.classList.toggle("solo", !!solo);
}
function paint(s) {
  $("sentence").textContent = s.sentence || "";
  $("foot").textContent = s.footnote || "";
  $("mode-one").setAttribute("aria-pressed", String(!!s.solo)); $("mode-all").setAttribute("aria-pressed", String(!s.solo));
  $("proc-live").textContent = s.process_line || "";
  // Warnings are listed behind the header "warnings" counter (app.js), not above the wall.
  $("warnings").innerHTML = "";
  cams = s.cameras || [];
  const empty = $("wall-empty");
  if (!s.ffmpeg) { empty.classList.remove("hide"); empty.textContent = "ffmpeg is not on PATH. Install it, then start Watch again."; }
  else if (!cams.length && !extraAll.length) { empty.classList.remove("hide"); empty.innerHTML = '<b>No sources connected.</b> Add a live camera, recording, or image folder. <a class="btn" href="/sources">Add a source</a>'; }
  else empty.classList.add("hide");
  render(false);
  for (const c of cams) {
    const node = $("tiles").querySelector(`.ct[data-cam="${CSS.escape(c.id)}"]`); if (!node) continue;
    node.querySelector(".ct-name").textContent = c.name;
    node.dataset.done = c.completed ? "1" : "";
    node.querySelector(".ct-tag").hidden = !(isRep(c.id) && !c.completed && node.dataset.vp === "1");
    if (node.dataset.vp !== "1") node.querySelector(".ct-fps").textContent = c.completed ? "done" : (c.feed || "");
    node.querySelector(".ct-stat").textContent = [c.status, c.skip].filter(Boolean).join(" · ");
    if (node.dataset.vp === "1") {} // people count comes from the interpolated boxes
    else node.querySelector(".ct-people").textContent = view === "live" && node.dataset.sig === "1" ? (c.people || "") : "";
    if (node.dataset.sig !== "0") node.querySelector(".dot").className = "dot " + (c.completed ? "" : "live");
    if (node.dataset.sig !== "1") node.querySelector(".ct-sig").textContent = node.dataset.vs === "loading" ? "Loading video…" : c.completed ? "No live preview — indexing complete." : "Waiting for first frame…";
    node.classList.toggle("flash", !!flash[c.id] && Date.now() < flash[c.id]);
  }
  if (alertApi === 0) paintAlerts();
}

// ---- alerts ----
function legacyAlerts() { return ((Watch.state && Watch.state.events) || []).map((e) => ({ legacy: true, id: e.id, rule: e.rule, camera: e.camera, note: e.note, status: e.status })); }
function noteNew(list) {
  const ids = list.map((a) => String(a.id));
  if (seen === null) { seen = new Set(ids); return; }
  for (const a of list) {
    if (seen.has(String(a.id))) continue; seen.add(String(a.id));
    const c = cams.find((c) => c.id === a.source_id || (a.camera && c.name && String(a.camera).startsWith(c.name)));
    if (c) { flash[c.id] = Date.now() + 5000; }
  }
}
function paintAlerts() {
  const list = alertApi === 0 ? legacyAlerts() : alerts;
  noteNew(list);
  const open = list.filter((a) => a.legacy ? a.status === "agreed" : a.decision === "unreviewed").length;
  $("alert-count").textContent = open; $("alert-count").className = "tag" + (open ? " bad" : "");
  const key = JSON.stringify(list); if (key === alertKey) return; alertKey = key;
  Watch.fill($("events"), list.map((a) => a.legacy
    ? `<div class="alert-row" data-cam-name="${esc(a.camera)}"><span class="badge ${a.status === "agreed" ? "warn" : ""}">${esc(a.status)}</span><span><b>${esc(a.rule)}</b><br><span class="quiet">${esc(a.camera)} ${esc(a.note || "")}</span></span><span class="row-actions">${a.status === "agreed" ? `<button type="button" class="sm" data-yes="${a.id}">Confirm</button><button type="button" class="sm" data-no="${a.id}">Reject</button>` : ""}</span></div>`
    : `<div class="alert-row" data-cam-id="${esc(a.source_id)}"><span class="badge ${a.state === "active" ? "bad" : "warn"}">${esc(a.state)}</span><span><b>${esc(a.rule_id)}</b> · ${esc(a.source_id)}<br><span class="quiet mono">${V.usTime(a.start_us)}–${a.end_us != null ? V.usTime(a.end_us) : "ongoing"} · ${esc(a.severity)} · ${esc(a.decision)}${a.actor ? " by " + esc(a.actor) : ""}</span></span><span class="row-actions"><a class="btn sm" href="/review?source=${encodeURIComponent(a.source_id)}&media=${encodeURIComponent(a.media_id || "")}&start_us=${a.start_us}&end_us=${a.end_us ?? a.start_us}">Review</a>${a.decision === "unreviewed" ? `<button type="button" class="sm" data-dec="confirmed" data-aid="${esc(a.id)}">Confirm</button><button type="button" class="sm" data-dec="rejected" data-aid="${esc(a.id)}">Reject</button>` : ""}</span></div>`).join("") || '<p class="quiet">No alerts. They appear when an enabled rule’s condition persists.</p>');
}
async function loadAlerts() {
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/alerts"));
    if (r.unavailable) alertApi = 0; else { alertApi = 1; alerts = V.list(r.data, "alerts"); if ($("alert-msg").classList.contains("bad")) V.clear($("alert-msg")); }
    paintAlerts();
  } catch (e) { V.show($("alert-msg"), e, loadAlerts); }
}
async function loadExtra() {
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/sources"));
    if (r.unavailable) { extra = []; extraKey = ""; return; }
    extraAll = V.list(r.data, "sources");
    const k = extraAll.map((s) => s.id + s.enabled).join(","); if (k !== extraKey) { extraKey = k; render(true); }
  } catch (_) {}
}
$("events").onclick = async (e) => {
  const t = e.target.closest("button");
  if (!t) { const row = e.target.closest(".alert-row"); if (row && !e.target.closest("a")) { const c = cams.find((c) => c.id === row.dataset.camId || (row.dataset.camName && c.name && row.dataset.camName.startsWith(c.name))); if (c) setSolo(c.id); } return; }
  t.disabled = true;
  try {
    if (t.dataset.aid) { await V.call("POST", `/api/v1/alerts/${encodeURIComponent(t.dataset.aid)}/decision`, { decision: t.dataset.dec }); V.ok($("alert-msg"), "Decision recorded: " + t.dataset.dec + "."); await loadAlerts(); }
    else { const id = Number(t.dataset.yes || t.dataset.no); await V.call("POST", "/api/confirm", { id, verdict: t.dataset.yes ? "confirm" : "reject" }); V.ok($("alert-msg"), "Decision recorded."); }
  } catch (err) { V.show($("alert-msg"), err); t.disabled = false; }
};
$("mode-one").onclick = () => V.call("POST", "/api/settings", { solo: true }).catch((e) => V.show($("alert-msg"), e));
$("mode-all").onclick = () => V.call("POST", "/api/settings", { solo: false }).catch((e) => V.show($("alert-msg"), e));
$("layouts").onclick = (e) => { const b = e.target.closest("button"); if (b) setLayout(b.dataset.l); };
$("pg-prev").onclick = () => { page = Math.max(0, page - 1); render(true); };
$("pg-next").onclick = () => { page += 1; render(true); };
$("fs").onclick = () => fullscreen();
$("rail-toggle").onclick = () => { const r = $("rail"); const hidden = r.classList.toggle("hide"); $("rail-toggle").setAttribute("aria-expanded", String(!hidden)); };
document.addEventListener("keydown", (e) => {
  if (e.target.closest("input, textarea, select") || e.ctrlKey || e.metaKey || e.altKey) return;
  const k = e.key.toLowerCase();
  if ("1234".includes(k) && k) setLayout(k); else if (k === "a") setLayout("auto"); else if (k === "f") fullscreen(); else if (k === "escape" && solo) setSolo("");
});
const frames = {}; // camera id -> { an, obs, img, dets, t }
let trails = true;
try { trails = localStorage.getItem("watch.trails") !== "0"; } catch (_) {}

// ---- video + synchronised boxes ("boxes" view) ----
const MAX_VIDEOS = 16, CHUNK = 30, LOOK = 2.5e6, HOLD = 600e3, FADE_IN = 150e3, MAX_GAP = 2.5e6;
const mediaCache = new Map(), bad = {}, lastT = {}; // media id -> MediaAsset; camera id -> time a failed video was seen; camera id -> last media seconds
let mediaOf = {}, mediaBusy = false;
// Sources the head replays from a recording on a loop ("file" kind): id -> start fraction. Position = (start*dur + wall clock) mod dur, the same for every tile and viewer.
var repOf = {};
// Recordings of the same scene differ by a fraction of a second in length, which over ~500k loops would put the cameras hours apart; so cameras of similar length share the shortest length as their loop period.
const period = (id, dur) => Object.keys(repOf).reduce((p, k) => { const o = mediaOf[k]; return o && o.dur < p && Math.abs(o.dur - dur) < dur * 0.01 ? o.dur : p; }, dur);
var repEpoch = 0; // server start time (unix s): a replayed source is at its `start` position at that moment
const repPos = (id, dur) => { const r = repOf[id]; if (!r) return null; const P = period(id, dur); return (((r.start * P + (Date.now() / 1000 - repEpoch)) % P) + P) % P; };
async function loadRep() {
  try {
    const j = await V.call("GET", "/api/sources"), next = {};
    if (j && Number(j.epoch) > 0) repEpoch = Number(j.epoch);
    for (const x of (j && j.expanded) || []) if (x.kind === "file") next[x.id] = { start: Number(x.start) || 0 };
    if (JSON.stringify(next) !== JSON.stringify(repOf)) { repOf = next; if (view === "live") render(true); }
  } catch (_) {}
}
// camera id (= source id) -> playable media {id, dur, running}. Newest job first; legacy-import jobs are ignored.
async function loadMedia() {
  if (mediaBusy) return; mediaBusy = true;
  try {
    const jobs = V.list(await V.call("GET", "/api/v1/jobs"), "jobs").filter((j) => String(j.id).startsWith("job_")).sort((a, b) => (b.created_utc_us || 0) - (a.created_utc_us || 0)); // legacy-import jobs point at media rows that may be gone (404)
    const ids = [...new Set(jobs.flatMap((j) => j.media_ids || []))];
    await Promise.all(ids.map(async (id) => { const m = mediaCache.get(id); if (m && m.availability === "available") return; try { mediaCache.set(id, await V.call("GET", "/api/v1/media/" + encodeURIComponent(id))); } catch (_) {} }));
    const best = {};
    for (const j of jobs) for (const id of j.media_ids || []) {
      const m = mediaCache.get(id);
      if (m && m.availability === "available" && m.duration_us > 0 && !m.still && !best[m.source_id]) best[m.source_id] = { id, dur: m.duration_us / 1e6, running: j.state === "running" };
    }
    mediaOf = best;
    document.querySelectorAll("#tiles .ct[data-cam]").forEach((el) => setupVideo(el));
  } catch (e) { console.warn("wall: media lookup failed", e); } finally { mediaBusy = false; }
}
const smooth = (k) => k * k * (3 - 2 * k);
// Boxes for media time t (us): interpolate between the two bracketing observations by track id; hold/fade a lone earlier
// track; fade in a lone later one; nothing when no observation is within LOOK of t. Detections without a track id cross-fade.
function boxesAt(obs, t) {
  let lo = 0, hi = obs.length; while (lo < hi) { const m = (lo + hi) >> 1; if (obs[m].t <= t) lo = m + 1; else hi = m; }
  const a = obs[lo - 1] && t - obs[lo - 1].t <= LOOK ? obs[lo - 1] : null, b = obs[lo] && obs[lo].t - t <= LOOK ? obs[lo] : null;
  if (!a && !b) return [];
  const pair = a && b && b.t - a.t <= MAX_GAP, e = pair ? smooth((t - a.t) / (b.t - a.t)) : 0, red = Watch.reduced();
  const bt = new Map(), used = new Set(), out = [];
  if (pair) for (const d of b.dets) if (d.track_id != null) bt.set(d.track_id, d);
  if (a) for (const d of a.dets) {
    const o = d.track_id != null ? bt.get(d.track_id) : null;
    if (o) { used.add(o); out.push({ ...d, confidence: o.confidence, bbox: red ? (e < 0.5 ? d.bbox : o.bbox) : d.bbox.map((v, i) => v + (o.bbox[i] - v) * e), alpha: 1 }); }
    else { const age = t - a.t; if (age < HOLD) out.push({ ...d, alpha: age < HOLD - 200e3 ? 1 : (HOLD - age) / 200e3 }); }
  }
  if (b) for (const d of b.dets) if (!used.has(d)) { const k = 1 - (b.t - t) / FADE_IN; if (k > 0) out.push({ ...d, alpha: Math.min(1, k) }); }
  return out;
}
async function loadChunk(vt, c) {
  const old = vt.chunks.get(c), now = performance.now();
  if (old && (old.busy || (now - old.at < (old.err ? 10000 : vt.m.running ? 8000 : 1e12)))) return;
  const ch = { busy: true, at: now, items: old ? old.items : [], err: false }; vt.chunks.set(c, ch);
  try {
    const r = await fetch(`/api/v1/media/${encodeURIComponent(vt.m.id)}/observations?start_us=${c * CHUNK * 1e6}&end_us=${(c + 1) * CHUNK * 1e6}`, { credentials: "same-origin" });
    if (!r.ok) throw 0;
    ch.items = V.list(await r.json(), "observations").map((o) => ({ t: o.observation.media_time_us, dets: o.detections || [] }));
  } catch (_) { ch.err = true; }
  ch.busy = false; ch.at = performance.now();
  // ponytail: all chunks are merged and re-sorted on arrival (fine for a few hundred observations per 30 s); upgrade to a per-chunk lookup if it shows up in profiles
  vt.obs = [...vt.chunks.values()].flatMap((x) => x.items).sort((p, q) => p.t - q.t).filter((o, i, arr) => !i || o.t !== arr[i - 1].t);
}
function ensureObs(vt, t) {
  const c = Math.floor(t / CHUNK);
  loadChunk(vt, c); if (t - c * CHUNK > 15 && (c + 1) * CHUNK < vt.m.dur) loadChunk(vt, c + 1);
}
const stopVideo = (v) => { try { v.pause(); v.removeAttribute("src"); v.load(); } catch (_) {} };
function teardown() {
  io.disconnect(); ro.disconnect();
  document.querySelectorAll("#tiles .ct").forEach((el) => { const vt = el._vt; if (!vt) return; vt.dead = true; try { if (vt.h && vt.v.cancelVideoFrameCallback) vt.v.cancelVideoFrameCallback(vt.h); } catch (_) {} stopVideo(vt.v); el._vt = null; });
}
function clearOv(el) {
  const vt = el._vt; if (!vt) return;
  vt.an.clear(); vt.mt = -1; vt.last = 0;
  if (!vt.blank) { vt.ov.getContext("2d").clearRect(0, 0, vt.ov.width, vt.ov.height); vt.blank = true; }
  const p = el.querySelector(".ct-people"); if (p) p.textContent = "";
}
function fallback(el, why) {
  const vt = el._vt; if (vt) { vt.dead = true; stopVideo(vt.v); clearOv(el); }
  el.dataset.vp = ""; el.dataset.vs = "";
  const n = el.querySelector(".ct-note"); n.textContent = why; n.hidden = false;
  const f = frames[el.dataset.cam]; if (f) draw(el, f);
}
function playState(el) {
  const vt = el._vt; if (!vt || vt.dead) return;
  const want = el._vis !== false && !document.hidden;
  if (want && vt.v.paused) vt.v.play().catch(() => {}); else if (!want && !vt.v.paused) vt.v.pause();
}
function setupVideo(el) {
  if (el._vt || !(view === "boxes" || (view === "live" && isRep(el.dataset.cam)))) return;
  const id = el.dataset.cam, m = mediaOf[id], v = el.querySelector("video");
  if (!m || !v) return; // nothing playable: stay on the analysed view
  if (bad[id] && Date.now() - bad[id] < 60000) { const n = el.querySelector(".ct-note"); n.textContent = "Video unavailable. Showing analysed frames."; n.hidden = false; return; }
  if (document.querySelectorAll("#tiles video[src]").length >= MAX_VIDEOS) return; // over the cap: analysed still
  const vt = el._vt = { m, v, ov: el.querySelector(".ov"), an: Watch.Annotate.create({ direct: true }), chunks: new Map(), obs: [], mt: -1, f0: 0, pf0: 0, fc: 0, blank: true, dead: false, sync: 0 };
  el.dataset.vs = "loading";
  v.addEventListener("loadedmetadata", () => {
    const rp = repPos(id, m.dur), t0 = rp != null ? rp : lastT[id] != null ? lastT[id] : frames[id] ? frames[id].t / 1e6 : 0;
    if (t0 > 0 && t0 < (v.duration || m.dur) - 1) v.currentTime = t0;
  });
  v.addEventListener("playing", () => { el.dataset.vp = "1"; el.dataset.vs = ""; el.dataset.sig = "1"; el.querySelector(".ct-sig").style.display = "none"; arm(el); });
  for (const ev of ["seeking", "waiting", "stalled", "pause", "emptied"]) v.addEventListener(ev, () => clearOv(el));
  v.addEventListener("error", () => { bad[id] = Date.now(); fallback(el, "Video unavailable. Showing analysed frames."); });
  v.src = "/api/v1/media/" + encodeURIComponent(m.id) + "/stream";
  playState(el);
}
function arm(el) {
  const vt = el._vt; if (!vt || vt.dead || vt.armed) return; vt.armed = true;
  const v = vt.v, cb = (now, md) => { vt.armed = false; if (vt.dead) return; frame(el, vt, now, md ? md.mediaTime : v.currentTime, md); arm(el); };
  vt.h = v.requestVideoFrameCallback ? v.requestVideoFrameCallback(cb) : requestAnimationFrame((t) => cb(t, null));
}
function frame(el, vt, now, mt, md) {
  if (vt.v.paused || vt.v.seeking) return;
  vt.last = now;
  if (repOf[el.dataset.cam] && now >= vt.sync) { // replayed "live" source: stay on the shared wall-clock position
    const dur = period(el.dataset.cam, vt.m.dur), d = ((mt - repPos(el.dataset.cam, vt.m.dur) + dur * 1.5) % dur) - dur / 2;
    vt.sync = now + 1000;
    if (Math.abs(d) > (vt.synced ? 1.5 : 0.5)) { vt.v.currentTime = repPos(el.dataset.cam, vt.m.dur) + 0.15; clearOv(el); vt.synced = true; vt.sync = now + 2500; return; }
    vt.synced = true;
  }
  if (vt.mt >= 0 && (mt < vt.mt - 0.05 || mt - vt.mt > 1)) vt.an.clear(); // seek or loop wrap: restart the overlay
  vt.mt = mt; lastT[el.dataset.cam] = mt;
  const pf = md && md.presentedFrames != null ? md.presentedFrames : (vt.pf0 + 1);
  if (!vt.f0) { vt.f0 = now; vt.pf0 = pf; }
  else if (now - vt.f0 >= 1000) {
    vt.fps = (pf - vt.pf0) * 1000 / (now - vt.f0); vt.f0 = now; vt.pf0 = pf;
    el.querySelector(".ct-fps").textContent = (el.dataset.done ? "done · " : "") + Math.round(vt.fps) + " fps";
  }
  if (el._vis === false) return;
  ensureObs(vt, mt);
  const dets = boxesAt(vt.obs, mt * 1e6);
  vt.an.update(dets, now);
  if (!dets.length && vt.blank) return;
  const sz = el._sz, v = vt.v; if (!sz || !v.videoWidth) return;
  const g = vt.ov.getContext("2d"), dpr = vt.ov.width / sz.w;
  g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, sz.w, sz.h);
  vt.an.paint(g, fitRect(sz.w, sz.h, v.videoWidth, v.videoHeight), now, { trail: trails, badge: false });
  vt.blank = !dets.length;
  const n = dets.filter((d) => d.class === "person" && d.alpha > 0.5).length;
  if (n !== vt.people) { vt.people = n; el.querySelector(".ct-people").textContent = n ? n + (n === 1 ? " person" : " people") : ""; }
}
const io = new IntersectionObserver((es) => es.forEach((e) => { e.target._vis = e.isIntersecting; playState(e.target); }));
const ro = new ResizeObserver((es) => es.forEach((e) => {
  const el = e.target, w = e.contentRect.width, h = e.contentRect.height; el._sz = { w, h };
  const vt = el._vt, d = window.devicePixelRatio || 1;
  if (vt) { vt.ov.width = Math.round(w * d); vt.ov.height = Math.round(h * d); vt.blank = true; } else draw(el, frames[el.dataset.cam]);
}));
document.addEventListener("visibilitychange", () => document.querySelectorAll("#tiles .ct").forEach(playState));
// a stalled video (no new frame for 0.8 s while "playing") must not keep stale boxes on screen
setInterval(() => document.querySelectorAll("#tiles .ct").forEach((el) => { const vt = el._vt; if (vt && !vt.dead && vt.last && performance.now() - vt.last > 800) clearOv(el); }), 400);
setLayout(layout);
V.auth(); loadAlerts(); setInterval(loadAlerts, 3000);
// ---- analyzed frames with their own boxes ----
function fitRect(cw, ch, iw, ih) {
  const k = Math.min(cw / iw, ch / ih), w = iw * k, h = ih * k;
  return { x: (cw - w) / 2, y: (ch - h) / 2, w, h };
}
function draw(el, f, now) {
  const cv = el.querySelector("canvas.base"); if (!cv || !f || !f.img) return;
  now = now ?? performance.now();
  const dpr = window.devicePixelRatio || 1, cw = cv.clientWidth, ch = cv.clientHeight;
  if (!cw || !ch) return;
  if (cv.width !== Math.round(cw * dpr) || cv.height !== Math.round(ch * dpr)) { cv.width = Math.round(cw * dpr); cv.height = Math.round(ch * dpr); }
  const g = cv.getContext("2d"); g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, cw, ch);
  const r = fitRect(cw, ch, f.img.naturalWidth, f.img.naturalHeight);
  g.drawImage(f.img, r.x, r.y, r.w, r.h);
  el.dataset.drawn = f.obs; el.dataset.sig = "1"; el.querySelector(".ct-sig").style.display = "none";
  // boxes are positioned in the image content rect (object-fit: contain), so they stay on the picture at any tile size
  f.an.paint(g, r, now, { trail: trails, bx: 8, by: 28 });
  const css = getComputedStyle(document.documentElement), ts = Math.max(0, Math.round(f.t / 1000)) / 1000;
  g.font = "10px " + (css.getPropertyValue("--font-mono") || "monospace"); g.textBaseline = "top"; g.fillStyle = "rgba(0,0,0,.6)";
  const stamp = "analyzed @ " + ts.toFixed(2) + " s";
  g.fillRect(cw - g.measureText(stamp).width - 10, ch - 34, g.measureText(stamp).width + 8, 14);
  g.fillStyle = "#c5cfcb"; g.fillText(stamp, cw - g.measureText(stamp).width - 6, ch - 33);
  if (f.an.busy) kick();
}
// 60 fps only while some tile is animating; idle otherwise.
let raf = 0;
function loop(now) {
  raf = 0;
  document.querySelectorAll("#tiles .ct[data-cam]").forEach((el) => { const f = frames[el.dataset.cam]; if (f && f.an.busy) draw(el, f, now); });
}
function kick() { if (!raf) raf = requestAnimationFrame(loop); }
let polling = false;
async function pollFrames() {
  if (polling || view === "live" || document.hidden) return;
  polling = true;
  try {
    const tiles = [...document.querySelectorAll("#tiles .ct[data-cam]")].filter((el) => el.dataset.vp !== "1");
    await Promise.all(tiles.map(async (el) => {
      const id = el.dataset.cam, n = el.querySelector(".ct-sig");
      let v;
      try {
        const r = await fetch("/api/v1/live/" + encodeURIComponent(id) + "/latest", { cache: "no-store", credentials: "same-origin" });
        if (r.status === 204 || !r.ok) { if (!frames[id]) { el.dataset.sig = ""; } return; }
        v = await r.json();
      } catch (_) { return; }
      const cur = frames[id];
      if (cur && cur.obs === v.observation_id) { if (el.dataset.drawn !== String(cur.obs)) draw(el, cur); return; }
      const img = new Image();
      img.onload = () => {
        const f = frames[id] || (frames[id] = { an: Watch.Annotate.create() });
        Object.assign(f, { obs: v.observation_id, img, dets: v.detections || [], t: v.media_time_us || 0 });
        f.an.update(f.dets);
        el.dataset.sig = "1"; n.style.display = "none"; draw(el, f);
      };
      img.src = v.frame;
    }));
  } finally { polling = false; }
}
function setView(v) {
  view = v; try { localStorage.setItem("watch.wallView2", v); } catch (_) {}
  for (const k of ["boxes", "sync", "live"]) $("view-" + k).setAttribute("aria-pressed", String(v === k));
  render(true);
}
$("view-boxes").onclick = () => setView("boxes");
$("view-sync").onclick = () => setView("sync");
$("view-live").onclick = () => setView("live");
setView(view);
document.addEventListener("keydown", (e) => { if ((e.key === "v" || e.key === "V") && !e.target.closest("input,textarea,select")) setView(view === "boxes" ? "sync" : view === "sync" ? "live" : "boxes"); });
window.addEventListener("resize", () => document.querySelectorAll("#tiles .ct[data-cam]").forEach((el) => draw(el, frames[el.dataset.cam])));
function setTrails(on) {
  trails = on; try { localStorage.setItem("watch.trails", on ? "1" : "0"); } catch (_) {}
  $("trails").setAttribute("aria-pressed", String(on));
  document.querySelectorAll("#tiles .ct[data-cam]").forEach((el) => draw(el, frames[el.dataset.cam]));
}
$("trails").onclick = () => setTrails(!trails);
$("trails").setAttribute("aria-pressed", String(trails));
setInterval(pollFrames, 250);

Watch.start(paint, 400);
loadExtra(); setInterval(loadExtra, 10000);
loadMedia(); setInterval(loadMedia, 15000); loadRep(); setInterval(loadRep, 15000);
