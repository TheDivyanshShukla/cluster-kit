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
// "sync": draw each analyzed frame together with its own detections (boxes always match the picture).
// "video": the head's preview stream; it is decoded separately, so no boxes are drawn on it.
let view = "sync";
try { view = localStorage.getItem("watch.wallView") || "sync"; } catch (_) {}
try { layout = localStorage.getItem("watch.wallLayout") || "auto"; } catch (_) {}

function slotCount(n) {
  if (solo) return { cols: 1, rows: 1, slots: 1 };
  const k = layout === "auto" ? 0 : Number(layout);
  if (k === 1) return { cols: 1, rows: 1, slots: 1 };
  if (k) return { cols: k, rows: k, slots: k * k };
  const c = Math.max(1, Math.ceil(Math.sqrt(Math.max(n, 1)))), r = Math.max(1, Math.ceil(n / c));
  return { cols: c, rows: r, slots: c * r };
}
function tileHtml(c) {
  return `<div class="ct" role="listitem" tabindex="0" data-cam="${esc(c.id)}" aria-label="${esc(c.name)}: click to solo, double-click for fullscreen">
    ${view === "video" ? `<img alt="" src="/api/live/${encodeURIComponent(c.id)}">` : `<canvas aria-hidden="true"></canvas>`}
    <div class="ct-sig">Waiting for first frame…</div>
    <div class="ct-top"><span class="dot"></span><b class="ct-name"></b><span class="ct-fps mono"></span></div>
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
  const view = list.slice(page * per, page * per + per);
  const key = [layout, solo, page, view.map((e) => (e.c ? e.c.id : "x:" + e.x.id)).join(","), g.cols, g.rows].join("|");
  const tiles = $("tiles");
  $("pager").hidden = pages < 2; $("pg-label").textContent = (page + 1) + "/" + pages;
  if (force || tiles.dataset.key !== key) {
    tiles.dataset.key = key;
    tiles.style.setProperty("--cols", g.cols); tiles.style.setProperty("--rows", g.rows);
    tiles.innerHTML = view.map((e) => e.c ? tileHtml(e.c) : noSignal((e.x.name || e.x.id) + (e.x.enabled === false ? " (disabled)" : " (no worker reports it)"))).join("") + Array(Math.max(0, per - view.length)).fill(noSignal("empty slot")).join("");
    tiles.querySelectorAll(".ct[data-cam]").forEach(wire);
  }
  tiles.classList.toggle("solo", !!solo);
}
function paint(s) {
  $("sentence").textContent = s.sentence || "";
  $("foot").textContent = s.footnote || "";
  $("mode-one").setAttribute("aria-pressed", String(!!s.solo)); $("mode-all").setAttribute("aria-pressed", String(!s.solo));
  $("proc-live").textContent = s.process_line || "";
  $("warnings").innerHTML = (s.warnings || []).map((w) => `<div class="warn">${esc(w)}</div>`).join("");
  cams = s.cameras || [];
  const empty = $("wall-empty");
  if (!s.ffmpeg) { empty.classList.remove("hide"); empty.textContent = "ffmpeg is not on PATH. Install it, then start Watch again."; }
  else if (!cams.length && !extraAll.length) { empty.classList.remove("hide"); empty.innerHTML = '<b>No sources connected.</b> Add a live camera, recording, or image folder. <a class="btn" href="/sources">Add a source</a>'; }
  else empty.classList.add("hide");
  render(false);
  for (const c of cams) {
    const node = $("tiles").querySelector(`.ct[data-cam="${CSS.escape(c.id)}"]`); if (!node) continue;
    node.querySelector(".ct-name").textContent = c.name;
    node.querySelector(".ct-fps").textContent = c.completed ? "done" : (c.feed || "");
    node.querySelector(".ct-stat").textContent = [c.status, c.skip].filter(Boolean).join(" · ");
    if (view !== "video") node.querySelector(".ct-people").textContent = "";
    else node.querySelector(".ct-people").textContent = node.dataset.sig === "1" ? (c.people || "") : "";
    if (node.dataset.sig !== "0") node.querySelector(".dot").className = "dot " + (c.completed ? "" : "live");
    if (node.dataset.sig !== "1") node.querySelector(".ct-sig").textContent = c.completed ? "No live preview — indexing complete." : "Waiting for first frame…";
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
setLayout(layout);
V.auth(); loadAlerts(); setInterval(loadAlerts, 3000);
// ---- analyzed frames with their own boxes ----
const frames = {}; // camera id -> { obs, img, dets, t }
function fitRect(cw, ch, iw, ih) {
  const k = Math.min(cw / iw, ch / ih), w = iw * k, h = ih * k;
  return { x: (cw - w) / 2, y: (ch - h) / 2, w, h };
}
let trails = true;
try { trails = localStorage.getItem("watch.trails") !== "0"; } catch (_) {}
function draw(el, f, now) {
  const cv = el.querySelector("canvas"); if (!cv || !f || !f.img) return;
  now = now ?? performance.now();
  const dpr = window.devicePixelRatio || 1, cw = cv.clientWidth, ch = cv.clientHeight;
  if (!cw || !ch) return;
  if (cv.width !== Math.round(cw * dpr) || cv.height !== Math.round(ch * dpr)) { cv.width = Math.round(cw * dpr); cv.height = Math.round(ch * dpr); }
  const g = cv.getContext("2d"); g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, cw, ch);
  const r = fitRect(cw, ch, f.img.naturalWidth, f.img.naturalHeight);
  g.drawImage(f.img, r.x, r.y, r.w, r.h);
  el.dataset.drawn = f.obs;
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
  if (polling || view !== "sync" || document.hidden) return;
  polling = true;
  try {
    const tiles = [...document.querySelectorAll("#tiles .ct[data-cam]")];
    await Promise.all(tiles.map(async (el) => {
      const id = el.dataset.cam, n = el.querySelector(".ct-sig");
      let v;
      try {
        const r = await fetch("/api/v1/live/" + encodeURIComponent(id) + "/latest", { cache: "no-store", credentials: "same-origin" });
        if (!r.ok) { if (!frames[id]) { el.dataset.sig = ""; } return; }
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
  view = v; try { localStorage.setItem("watch.wallView", v); } catch (_) {}
  $("view-sync").setAttribute("aria-pressed", String(v === "sync")); $("view-video").setAttribute("aria-pressed", String(v === "video"));
  render(true);
}
$("view-sync").onclick = () => setView("sync");
$("view-video").onclick = () => setView("video");
setView(view);
document.addEventListener("keydown", (e) => { if ((e.key === "v" || e.key === "V") && !e.target.closest("input,textarea,select")) setView(view === "sync" ? "video" : "sync"); });
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
