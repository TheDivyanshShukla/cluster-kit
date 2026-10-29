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

const $ = Watch.$, esc = Watch.esc, num = Watch.num;
const clone = (o) => JSON.parse(JSON.stringify(o));
const sig = (o) => JSON.stringify(o);
const near = (a, b) => (typeof a === "number" && typeof b === "number") ? Math.abs(a - b) < 1e-9 || (isNaN(a) && isNaN(b)) : a === b;

// ================= detection pipeline =================
const FIELDS = [
  { k: "blur_on", g: "Skip cheaply", kind: "sw", base: 1, label: "Skip blurry frames", help: "Frames below the sharpness threshold are not analyzed. Cheap, and blurry frames rarely detect well anyway." },
  { k: "blur", g: "Skip cheaply", kind: "range", base: 1, label: "Blur threshold", min: 1, max: 400, step: 1, dep: "blur_on", help: "Sharpness score a frame must reach. Higher is stricter: more frames skipped." },
  { k: "blank_on", g: "Skip cheaply", kind: "sw", base: 1, label: "Skip blank pictures", help: "Black, white or flat frames cannot contain anything to detect." },
  { k: "same_on", g: "Skip cheaply", kind: "sw", base: 1, label: "Skip pictures that have not changed", help: "A frame nearly identical to the last analyzed one reuses the previous result." },
  { k: "same", g: "Skip cheaply", kind: "range", base: 1, label: "Change needed", unit: "%", min: 0.2, max: 40, step: 0.1, dep: "same_on", help: "How much the picture must change before it counts as new. Higher skips more." },
  { k: "detect_mode", g: "Detect", kind: "seg", label: "Detector input", opts: [["motion", "Motion regions"], ["full", "Full frame"]], help: "Motion regions runs the detector only on crops where something moved, plus periodic full keyframes. Full frame analyzes every picture entirely." },
  { k: "motion_sensitivity", g: "Detect", kind: "range", label: "Motion sensitivity", min: 0, max: 1, step: 0.05, dep: "motion", help: "Higher notices smaller or fainter movement (more regions, more ops)." },
  { k: "motion_min_area", g: "Detect", kind: "range", label: "Smallest moving area", unit: "of frame", min: 0, max: 0.25, step: 0.001, dep: "motion", help: "Ignore movement smaller than this fraction of the frame (noise, leaves, compression flicker)." },
  { k: "region_margin", g: "Detect", kind: "range", label: "Region margin", unit: "of region", min: 0, max: 1, step: 0.01, dep: "motion", help: "Extra context added around each moving region so objects at the edge are not cut off." },
  { k: "max_regions", g: "Detect", kind: "range", label: "Max regions per frame", unit: "crops", min: 1, max: 64, step: 1, dep: "motion", help: "Upper bound on crops run per frame. Beyond it the whole frame is analyzed instead." },
  { k: "detect_interval", g: "Detect", kind: "range", label: "Detector interval", unit: "frames", min: 1, max: 30, step: 1, dep: "motion", help: "Run the detector every Nth analyzed frame; tracking carries boxes in between." },
  { k: "keyframe_secs", g: "Detect", kind: "range", label: "Full keyframe every", unit: "seconds", min: 0.5, max: 60, step: 0.5, dep: "motion", help: "A full-frame pass at least this often, so objects that stopped moving are not lost." },
  { k: "every", g: "Throughput", kind: "seg", base: 1, label: "Look every", opts: [[1, "Every sharp frame"], [2, "2nd"], [3, "3rd"], [4, "4th"]], help: "Analyze one sharp frame out of N." },
  { k: "width", g: "Throughput", kind: "seg", base: 1, label: "Detector picture size", opts: [[320, "320"], [416, "416"], [512, "512"], [640, "640"], [960, "960"], [1280, "1280"]], help: "Width the picture is decoded at. In motion mode crops are cut from it at full detail, so a wider picture finds small objects (aerial, far cameras) at the cost of decoding. The detector itself sees its own input size." },
  { k: "rate", g: "Throughput", kind: "seg", base: 1, label: "Wall rate", opts: [[30, "30 fps"], [60, "60 fps"]], help: "Refresh rate of the live wall preview." }
];
const FBY = Object.fromEntries(FIELDS.map((f) => [f.k, f]));
const DEFAULTS = { mode: "balanced", blur_on: true, blur: 40, blank_on: true, same_on: true, same: 3.5, every: 2, width: 512, rate: 60 };
const PRESETS = {
  speed: { name: "Fast", blurb: "Fewest passes. Best for many cameras on one PC.", v: { blur_on: true, blur: 70, blank_on: true, same_on: true, same: 6, every: 3, width: 416, detect_mode: "motion", motion_sensitivity: 0.35, motion_min_area: 0.02, region_margin: 0.1, max_regions: 4, detect_interval: 4, keyframe_secs: 6 } },
  balanced: { name: "Balanced", blurb: "Good default for a few cameras.", v: { blur_on: true, blur: 40, blank_on: true, same_on: true, same: 3.5, every: 2, width: 512, detect_mode: "motion", motion_sensitivity: 0.5, motion_min_area: 0.01, region_margin: 0.15, max_regions: 8, detect_interval: 2, keyframe_secs: 3 } },
  accuracy: { name: "Accurate", blurb: "Looks at everything it can. Slowest, fewest misses.", v: { blur_on: true, blur: 18, blank_on: true, same_on: true, same: 1.2, every: 1, width: 640, detect_mode: "motion", motion_sensitivity: 0.75, motion_min_area: 0.004, region_margin: 0.25, max_regions: 16, detect_interval: 1, keyframe_secs: 1.5 } }
};
// Relative detector cost vs analyzing every frame whole at 640 px (= 1). A rough estimate from settings only.
function cost(p) {
  const w = (Number(p.width) || 512) / 640, base = (w * w) / (Number(p.every) || 1);
  if (p.detect_mode !== "motion") return base;
  const share = Math.min(1, 0.12 + 0.05 * (Number(p.max_regions) || 8) * (1 + 2 * (Number(p.region_margin) || 0.15)));
  return base * share / (Number(p.detect_interval) || 1);
}
const savedPct = (p) => Math.max(0, Math.min(99, Math.round((1 - cost(p)) * 100)));

let saved = {}, draft = {}, psig = "", keysSig = "", ferr = {};
let mdraft = [], mbase = [], msig = "", rbase = {}, renab = {}, rsig = "";
let models = [], entries = [], v1Models = false, sources = [], editingRule = null, lineTouched = false, ed;

const pipeKeys = () => FIELDS.filter((f) => f.base || f.k in saved).map((f) => f.k);
const pipeDirty = () => pipeKeys().filter((k) => !near(draft[k], saved[k]));
const pick = (m) => ({ id: m.id, enabled: !!m.enabled, model: m.model || "", labels: (m.labels || []).slice(), conf: Number(m.conf) || 0, gflops: Number(m.gflops) || 0, confirm: Number(m.confirm) || 1 });
const modDirty = () => mdraft.filter((m, i) => sig(m) !== sig(mbase[i])).length;
const ruleDirty = () => Object.keys(renab).filter((id) => renab[id] !== rbase[id]).length;
const pct = (v, lo, hi) => Math.max(0, Math.min(100, ((v - lo) / (hi - lo)) * 100)) || 0;

function fieldHtml(f) {
  const id = "f-" + f.k, hp = `<span class="f-help" role="tooltip" id="h-${f.k}">${esc(f.help)}</span>`, er = `<p class="f-err" id="e-${f.k}"></p>`;
  if (f.kind === "sw") return `<div class="f f-sw" data-k="${f.k}"><label class="sw"><input type="checkbox" id="${id}" aria-describedby="h-${f.k}"><span class="sw-t"></span><span class="sw-l">${esc(f.label)}</span></label>${hp}</div>`;
  if (f.kind === "seg") return `<div class="f" data-k="${f.k}"><div class="f-top"><span class="f-lab" id="l-${f.k}">${esc(f.label)}</span></div><div class="seg" role="radiogroup" aria-labelledby="l-${f.k}" aria-describedby="h-${f.k}">${f.opts.map(([v, t]) => `<button type="button" role="radio" data-v="${v}" aria-checked="false">${esc(t)}</button>`).join("")}</div>${hp}</div>`;
  return `<div class="f" data-k="${f.k}"><div class="f-top"><label for="${id}">${esc(f.label)}</label><span class="f-unit">${esc(f.unit || "")}</span></div>
    <div class="f-in"><input type="range" class="rng" id="${id}-r" min="${f.min}" max="${f.max}" step="${f.step}" aria-label="${esc(f.label)} slider" tabindex="-1"><input type="number" id="${id}" min="${f.min}" max="${f.max}" step="${f.step}" aria-describedby="h-${f.k} e-${f.k}"></div>${hp}${er}</div>`;
}
function buildPipe() {
  const keys = pipeKeys(), groups = [];
  for (const f of FIELDS) { if (!keys.includes(f.k)) continue; let g = groups.find((x) => x[0] === f.g); if (!g) groups.push(g = [f.g, []]); g[1].push(f); }
  $("pipe-fields").innerHTML = groups.map(([name, fs]) => `<fieldset class="fgroup"><legend>${esc(name)}</legend><div class="fgrid">${fs.map(fieldHtml).join("")}</div></fieldset>`).join("");
  $("pipe-fields").classList.remove("enter"); void $("pipe-fields").offsetWidth; if (!Watch.reduced()) $("pipe-fields").classList.add("enter");
  if (!$("presets").firstChild) {
    $("presets").innerHTML = Object.entries(PRESETS).map(([id, p]) => `<button type="button" class="pcard" role="radio" data-p="${id}" aria-checked="false"><b>${p.name}</b><span class="pc-d">${esc(p.blurb)}</span><span class="pc-n">about <b class="pn">0</b>% fewer ops</span></button>`).join("");
    $("est").innerHTML = '<div class="est-n"><b id="est-pct">0</b><span>% fewer detector ops with these settings, against analyzing every frame whole at 640 px</span></div><div class="est-bar"><i id="est-bar"></i></div><p class="field-help">A rough estimate from the settings alone. Measured numbers per camera appear below once the engine reports them.</p>';
  }
}
function validate(k) {
  const f = FBY[k]; if (!f || f.kind !== "range") return "";
  const v = draft[k]; if (!Number.isFinite(v)) return "Enter a number.";
  if (v < f.min || v > f.max) return `Enter a value from ${f.min} to ${f.max}.`;
  if (f.step >= 1 && !Number.isInteger(v)) return "Use a whole number.";
  return "";
}
function syncPipe() {
  const motionOff = draft.detect_mode === "full";
  for (const k of pipeKeys()) {
    const f = FBY[k], el = document.querySelector(`.f[data-k="${k}"]`); if (!el) continue;
    const off = f.dep === "motion" ? motionOff : f.dep ? draft[f.dep] === false : false;
    el.classList.toggle("off", !!off);
    const err = validate(k) || ferr[k] || ""; el.classList.toggle("bad", !!err);
    if (f.kind === "sw") { el.querySelector("input").checked = !!draft[k]; }
    else if (f.kind === "seg") { el.querySelectorAll("button").forEach((b) => b.setAttribute("aria-checked", String(String(b.dataset.v) === String(draft[k])))); }
    else {
      const n = el.querySelector('input[type="number"]'), r = el.querySelector(".rng");
      if (document.activeElement !== n) n.value = Number.isFinite(draft[k]) ? num(draft[k]) : "";
      if (Number.isFinite(draft[k])) { r.value = draft[k]; r.style.setProperty("--p", pct(draft[k], f.min, f.max) + "%"); }
      n.disabled = r.disabled = !!off; n.setAttribute("aria-invalid", String(!!err));
      const e = el.querySelector(".f-err"); if (e.textContent !== err) e.textContent = err;
    }
    el.classList.toggle("changed", !near(draft[k], saved[k]));
  }
  document.querySelectorAll("#presets .pcard").forEach((b) => {
    b.setAttribute("aria-checked", String(draft.mode === b.dataset.p));
    Watch.count(b.querySelector(".pn"), savedPct({ ...draft, ...PRESETS[b.dataset.p].v, detect_mode: "detect_mode" in saved ? PRESETS[b.dataset.p].v.detect_mode : undefined }));
  });
  const p = savedPct(draft); Watch.count($("est-pct"), p); $("est-bar").style.transform = `scaleX(${p / 100})`;
}
function setDraft(k, v) { draft[k] = v; delete ferr[k]; draft.mode = "custom"; touch(); }
$("pipe-fields").addEventListener("input", (e) => {
  const el = e.target.closest(".f"); if (!el) return; const k = el.dataset.k, f = FBY[k];
  if (f.kind === "sw") setDraft(k, e.target.checked);
  else if (e.target.matches('input[type="number"]')) setDraft(k, e.target.value === "" ? NaN : Number(e.target.value));
  else if (e.target.matches(".rng")) setDraft(k, Number(e.target.value));
});
$("pipe-fields").addEventListener("click", (e) => {
  const b = e.target.closest("[data-v]"); if (!b) return; const k = b.closest(".f").dataset.k, raw = b.dataset.v;
  setDraft(k, FBY[k].opts.some(([v]) => typeof v === "number") ? Number(raw) : raw);
});
$("presets").addEventListener("click", (e) => {
  const b = e.target.closest("[data-p]"); if (!b) return; const p = PRESETS[b.dataset.p];
  for (const k of Object.keys(p.v)) if (pipeKeys().includes(k)) draft[k] = p.v[k];
  ferr = {}; draft.mode = b.dataset.p; touch();
});

// measured savings (only when the engine reports the stats)
function paintSaved(s) {
  const cams = (s.cameras || []).filter((c) => Number(c.pixels_full) > 0 && c.pixels_inferred != null), box = $("saved-ops");
  box.hidden = !cams.length; if (!cams.length) return;
  const key = cams.map((c) => c.id).join(",");
  if (box._k !== key) {
    box._k = key;
    box.innerHTML = '<div class="su-sh sub"><h3>Measured on your cameras</h3><p>Pixels given to the detector against analyzing every frame whole.</p></div>' + cams.map((c) => `<div class="so" data-c="${esc(c.id)}"><div class="so-top"><b>${esc(c.name || c.id)}</b><span class="mono"><b class="so-p">0</b>% saved</span></div><div class="est-bar"><i></i></div><div class="kv"><span>detector passes <b data-f="detector_passes">0</b></span><span>static skipped <b data-f="static">0</b></span><span>regions <b data-f="regions">0</b></span><span>keyframes <b data-f="keyframes">0</b></span></div></div>`).join("");
  }
  for (const c of cams) {
    const el = box.querySelector(`.so[data-c="${CSS.escape(c.id)}"]`); if (!el) continue;
    const p = Math.max(0, Math.min(100, (1 - Number(c.pixels_inferred) / Number(c.pixels_full)) * 100));
    Watch.count(el.querySelector(".so-p"), Math.round(p)); el.querySelector(".est-bar i").style.transform = `scaleX(${p / 100})`;
    el.querySelectorAll("[data-f]").forEach((n) => { if (c[n.dataset.f] != null) Watch.count(n, Number(c[n.dataset.f])); });
  }
}

// ================= models =================
const SUPPORT = { near: "Spatial proximity heuristic; it does not prove holding.", holding: "No installed module declares holding." };
function paintCaps() {
  const box = $("cap-list");
  if (!v1Models) { Watch.fill(box, '<div class="unavailable">Model versions, providers and validation status are not reported by this server version. Showing the configured modules only.</div>'); return; }
  if (!models.length) { Watch.fill(box, '<div class="empty-state"><h2>No models installed</h2><p>Register a detection module below. Until a model is installed, no class can be searched.</p></div>'); return; }
  const have = new Set(models.flatMap((m) => m.supported_relations || [])), classes = new Set(models.flatMap((m) => m.labels || []));
  const holding = have.has("holding");
  Watch.fill(box, `<div class="capsum"><div><b>${classes.size}</b><span>classes known</span></div><div><span class="badge warn">approximate</span><span>near (box proximity)</span></div><div><span class="badge ${holding ? "ok" : "bad"}">${holding ? "supported" : "unsupported"}</span><span>holding</span></div></div>`);
}
function modCard(m, i) {
  const en = entries.find((x) => x.module && x.module.id === m.id), mv = en ? en.model : models.find((x) => x.id === m.id), d = mdraft[i] || pick(m);
  const rels = mv ? (mv.supported_relations || []) : [];
  const labels = (m.labels || []).slice(0, 400);
  return `<article class="mcard${d.enabled ? " on" : ""}" data-i="${i}">
    <header><label class="sw"><input type="checkbox" data-f="enabled" ${d.enabled ? "checked" : ""} aria-label="Enable ${esc(m.name)}"><span class="sw-t"></span></label><h3>${esc(m.name)}</h3>
      <span class="mbadges">${mv ? `<span class="badge ${mv.validation === "unvalidated" ? "warn" : "ok"}" title="${mv.validation === "unvalidated" ? "No labeled evaluation exists for this model on your footage. Confidence is not accuracy." : "Validated"}">${esc(mv.validation)}</span><span class="badge" title="Provider">${esc(mv.provider)}</span>${mv.input_size ? `<span class="badge">${mv.input_size}px</span>` : ""}` : (v1Models ? `<span class="badge warn">${esc(en && en.status || "no validated version")}</span>` : "")}${rels.length ? rels.map((r) => `<span class="badge ${r === "holding" ? "ok" : "warn"}" title="${esc(SUPPORT[r] || r)}">${esc(r)}</span>`).join("") : ""}</span></header>
    <div class="model-labels" id="ml-${i}">${labels.map((l) => `<span class="lbl">${esc(l)}</span>`).join("") || '<span class="quiet">no labels</span>'}</div>${labels.length > 10 ? `<button type="button" class="sm" data-more="${i}">Show all ${labels.length} classes</button>` : ""}
    <div class="f2 mfields">
      <div><label>Model path<input data-f="model" value="${esc(d.model)}"></label></div>
      <div><label>Labels (model output order)<input data-f="labels" value="${esc(d.labels.join(", "))}"></label></div>
      <div><label>Confidence<span class="f-in"><input type="range" class="rng" data-f="conf" min="0" max="1" step="0.05" value="${d.conf}" style="--p:${pct(d.conf, 0, 1)}%" aria-label="Confidence slider" tabindex="-1"><input type="number" data-f="conf" min="0" max="1" step="0.05" value="${num(d.conf)}"></span></label></div>
      <div><label>Hold frames<input type="number" data-f="confirm" min="1" value="${d.confirm}"></label></div>
      <div><label>Rough size, gigaflops<input type="number" data-f="gflops" min="0" step="0.1" value="${num(d.gflops)}"></label></div>
    </div>
    ${mv && mv.validation === "unvalidated" ? '<p class="field-help">Unvalidated: confidence is not accuracy.</p>' : ""}
    <div class="row"><button type="button" class="sm" data-rm="${i}">Remove module</button></div></article>`;
}
function renderModels() { Watch.fill($("mod-list"), (Watch.state?.modules || []).map(modCard).join("") || '<p class="quiet">No detection modules configured.</p>'); $("class-list").innerHTML = [...new Set((Watch.state?.modules || []).flatMap((m) => m.labels || []))].sort().map((c) => `<option value="${esc(c)}">`).join(""); }
$("mod-list").addEventListener("input", (e) => {
  const card = e.target.closest(".mcard"), f = e.target.dataset.f; if (!card || !f) return; const d = mdraft[Number(card.dataset.i)]; if (!d) return;
  if (f === "enabled") { d.enabled = e.target.checked; card.classList.toggle("on", d.enabled); }
  else if (f === "model") d.model = e.target.value.trim();
  else if (f === "labels") d.labels = e.target.value.split(",").map((s) => s.trim()).filter(Boolean);
  else { d[f] = Number(e.target.value); card.querySelectorAll(`[data-f="${f}"]`).forEach((n) => { if (n !== e.target) n.value = e.target.value; }); const r = card.querySelector(`.rng[data-f="${f}"]`); if (r) r.style.setProperty("--p", pct(d[f], 0, 1) + "%"); }
  touch();
});
$("mod-list").addEventListener("click", async (e) => {
  const more = e.target.closest("[data-more]");
  if (more) { const el = $("ml-" + more.dataset.more); el.classList.toggle("open"); more.textContent = el.classList.contains("open") ? "Show fewer" : "Show all classes"; return; }
  const b = e.target.closest("[data-rm]"); if (!b) return;
  const mods = Watch.state.modules || [], m = mods[Number(b.dataset.rm)]; if (!m) return;
  if (!confirm(`Remove module “${m.name}”? Its classes stop being detected on new frames; existing results stay.`)) return;
  b.disabled = true;
  try { await V.call("POST", "/api/modules", mods.filter((x) => x.id !== m.id)); Watch.state.modules = mods.filter((x) => x.id !== m.id); msig = ""; toast(`Removed module “${m.name}”.`); loadModels(); }
  catch (err) { V.show($("m-err"), err); b.disabled = false; $("m-err").closest("details").open = true; }
});
async function loadModels() {
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/models"));
    v1Models = !r.unavailable; entries = v1Models ? V.list(r.data, "models") : []; models = v1Models ? V.modelsOf(r.data) : [];
  } catch (e) { v1Models = false; V.show($("m-err"), e, loadModels); }
  paintCaps(); msig = ""; if (Watch.state) ingest(Watch.state);
}
$("add-module").onclick = async () => {
  const name = $("new-mod-name").value.trim(), id = $("new-mod-id").value.trim(), model = $("new-mod-path").value.trim(), labels = $("new-mod-labels").value.split(",").map((x) => x.trim()).filter(Boolean);
  if (!name || !/^[a-z0-9-]+$/.test(id) || !model || !labels.length) { V.show($("m-err"), { message: "Enter a name, lowercase module ID, ONNX path, and ordered labels." }); return; }
  const list = Watch.state?.modules || [];
  if (list.some((m) => m.id === id)) { V.show($("m-err"), { message: "That module ID already exists. Choose another ID." }); return; }
  $("add-module").disabled = true;
  try { await V.call("POST", "/api/modules", list.concat([{ id, name, kind: "object", model, labels, conf: 0.4, confirm: 3, gflops: 0, enabled: false }])); V.ok($("m-err"), "Module registered, disabled. Install the model on each worker, then enable it."); msig = ""; loadModels(); }
  catch (e) { V.show($("m-err"), e); }
  finally { $("add-module").disabled = false; }
};

// ================= rules =================
function renderRules() {
  const rules = Watch.state?.rules || [];
  Watch.fill($("rule-list"), rules.map((r, i) => `<article class="rcard${renab[r.id] ? " on" : ""}" data-i="${i}"><header><label class="sw"><input type="checkbox" data-rule="${i}" ${renab[r.id] ? "checked" : ""} aria-label="Enable ${esc(r.name)}"><span class="sw-t"></span></label><h3>${esc(r.name)}</h3>
    <span class="kv"><span>camera <b>${esc(r.camera || "each")}</b></span><span>hold <b>${r.hold} frames</b></span></span></header>
    <pre class="rline">${Watch.Editor.highlight(r.line, "lua")}</pre>
    <div class="row-actions"><button type="button" class="sm" data-edit="${i}">Edit</button><button type="button" class="sm" data-bt="${i}">Backtest</button><button type="button" class="sm" data-del="${i}">Remove</button></div></article>`).join("") || '<p class="quiet">No rules yet. Add one below.</p>');
}
const lineFor = () => Watch.lineFor($("r-class").value, $("r-op").value, $("r-n").value);
["r-class", "r-op", "r-n"].forEach((id) => $(id).addEventListener("input", () => {
  $("r-n").disabled = $("r-op").value === "seen";
  if (!lineTouched) ed.set(lineFor());
}));
const eline = () => ed.get().trim();
$("ed-rule").addEventListener("ed-input", () => { lineTouched = true; $("e-line").textContent = ""; });
$("r-name").addEventListener("input", () => { $("e-name").textContent = ""; });
$("r-hold").addEventListener("input", () => { $("e-hold").textContent = ""; });
function resetRuleForm() {
  editingRule = null; lineTouched = false; $("rule-form").reset(); $("r-n").disabled = false; ed.set(lineFor());
  $("rule-legend").textContent = "New rule"; $("rule-submit").textContent = "Add rule"; $("rule-cancel").hidden = true;
  ["e-name", "e-line", "e-hold"].forEach((i) => ($(i).textContent = ""));
}
function ruleFromForm() {
  const name = $("r-name").value.trim(), id = editingRule ? editingRule.id : (name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "rule");
  return { id, name, camera: $("r-cam").value.trim(), line: eline(), hold: Number($("r-hold").value) || 0, enabled: editingRule ? editingRule.enabled : true };
}
function ruleErrors(r) {
  let bad = 0;
  if (!r.name) { $("e-name").textContent = "Give the rule a name."; bad++; }
  if (!r.line) { $("e-line").textContent = "Enter a rule line, for example count(\"person\") > 3."; bad++; }
  if (!(r.hold >= 1) || !Number.isInteger(r.hold)) { $("e-hold").textContent = "Whole number, at least 1."; bad++; }
  return bad;
}
async function saveRules(rules, okText) {
  try { await V.call("POST", "/api/rules", rules); Watch.state.rules = rules; rsig = ""; rbase = Object.fromEntries(rules.map((r) => [r.id, r.enabled])); renab = { ...rbase }; toast(okText); ingest(Watch.state); return true; }
  catch (e) { V.show($("r-err"), e); return false; }
}
const curRules = () => (Watch.state.rules || []).map((r) => ({ ...r, enabled: r.id in renab ? renab[r.id] : r.enabled }));
$("rule-form").onsubmit = async (e) => {
  e.preventDefault(); V.clear($("r-err"));
  const rule = ruleFromForm(); if (ruleErrors(rule)) { (document.querySelector(".f-err:not(:empty)")?.closest("div")?.querySelector("input"))?.focus(); return; }
  const rules = curRules();
  if (editingRule) { const i = rules.findIndex((r) => r.id === editingRule.id); if (i >= 0) rules[i] = rule; else rules.push(rule); }
  else { let id = rule.id; while (rules.some((r) => r.id === id)) id += "-2"; rule.id = id; rules.push(rule); }
  const was = !!editingRule; $("rule-submit").disabled = true;
  if (await saveRules(rules, was ? "Rule saved. Workers pick it up on their next configuration reload." : "Rule added. It will be evaluated by the assigned workers.")) resetRuleForm();
  $("rule-submit").disabled = false;
};
$("rule-cancel").onclick = () => { resetRuleForm(); V.clear($("r-err")); };
$("rule-list").addEventListener("input", (e) => { const i = e.target.dataset.rule; if (i == null) return; const r = Watch.state.rules[Number(i)]; renab[r.id] = e.target.checked; e.target.closest(".rcard").classList.toggle("on", e.target.checked); touch(); });
$("rule-list").addEventListener("click", async (e) => {
  const t = e.target.closest("button"); if (!t) return; const rules = Watch.state.rules || [];
  if (t.dataset.edit != null) {
    const r = rules[Number(t.dataset.edit)]; if (!r) return;
    editingRule = r; lineTouched = true; $("r-name").value = r.name; $("r-cam").value = r.camera || ""; ed.set(r.line); $("r-hold").value = r.hold;
    $("rule-legend").textContent = "Edit rule: " + r.name; $("rule-submit").textContent = "Save rule"; $("rule-cancel").hidden = false;
    $("rule-form").scrollIntoView({ behavior: Watch.reduced() ? "auto" : "smooth", block: "center" }); $("r-name").focus({ preventScroll: true });
  } else if (t.dataset.del != null) {
    const r = rules[Number(t.dataset.del)]; if (!r) return;
    if (!confirm(`Remove rule “${r.name}”? Existing alert history is kept.`)) return;
    await saveRules(curRules().filter((x) => x.id !== r.id), `Removed rule “${r.name}”.`);
  } else if (t.dataset.bt != null) { const r = rules[Number(t.dataset.bt)]; if (r) backtest(r); }
});

// ---- backtest (results inline, each window links into Review) ----
async function loadSources() {
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/sources"));
    sources = r.unavailable ? [] : V.list(r.data, "sources");
    $("bt-sources").innerHTML = r.unavailable ? '<span class="quiet">Backtesting is not available on this server version.</span>' : (sources.map((s) => `<label><input type="checkbox" value="${esc(s.id)}"> ${esc(s.name || s.id)}</label>`).join("") || '<span class="quiet">No sources; all analyzed media would be used.</span>');
    $("rule-backtest").disabled = !!r.unavailable;
  } catch (e) { $("bt-sources").innerHTML = '<span class="quiet">Sources could not be loaded.</span>'; }
}
$("rule-backtest").onclick = () => { const r = ruleFromForm(); if (!r.line) { $("e-line").textContent = "Enter a rule line to backtest."; return; } backtest(r); };
async function backtest(rule) {
  const box = $("bt-result"), ids = [...document.querySelectorAll("#bt-sources input:checked")].map((i) => i.value);
  const f = Watch.fromLocal($("bt-from").value), t = Watch.fromLocal($("bt-to").value);
  box.innerHTML = '<div class="skel skel-card"></div>'; V.clear($("r-err")); box.scrollIntoView({ block: "nearest", behavior: Watch.reduced() ? "auto" : "smooth" });
  try {
    const j = await V.call("POST", "/api/v1/rules/backtest", { rule: { id: rule.id || "draft", name: rule.name, camera: rule.camera || "", line: rule.line, hold: rule.hold, enabled: true }, scope: { source_ids: ids, media_ids: [], from_utc_us: f ? Math.round(f * 1e6) : null, to_utc_us: t ? Math.round(t * 1e6) : null } });
    const alerts = V.list(j, "alerts"), run = j && (j.query_run_id || (j.run && j.run.id));
    box.innerHTML = `<div class="card bt"><h3>${alerts.length} alert window${alerts.length === 1 ? "" : "s"} for “${esc(rule.name || "draft rule")}”</h3>` + (alerts.length ? alerts.map((a) => `<div class="alert-row"><span class="badge ${a.severity === "high" ? "bad" : "warn"}">${esc(a.severity || "alert")}</span><div><b>${esc(a.source_id)}</b> · ${V.usTime(a.start_us)} – ${a.end_us != null ? V.usTime(a.end_us) : "ongoing"}<br><span class="quiet mono">${esc(JSON.stringify(a.values || {})).slice(0, 140)}</span></div><a class="btn sm" href="${esc(run ? "/review?run=" + encodeURIComponent(run) : "/review?source=" + encodeURIComponent(a.source_id) + "&media=" + encodeURIComponent(a.media_id || "") + "&start_us=" + a.start_us + "&end_us=" + (a.end_us ?? a.start_us))}">Open in review</a></div>`).join("") : '<p class="quiet">No alert windows. That is not proof the rule is wrong or the condition never happened: only frames already analyzed were evaluated.</p>') + (j && j.coverage ? `<div class="kv"><span>inferred <b>${V.n(j.coverage.inferred)}</b></span><span>skipped <b>${V.n(j.coverage.skipped)}</b></span><span>failed <b>${V.n(j.coverage.failed)}</b></span></div>` : "") + "</div>";
  } catch (e) {
    if (V.absent(e)) box.innerHTML = '<div class="unavailable">Backtesting is not available on this server version.</div>';
    else { box.innerHTML = '<div class="msg" id="bt-err"></div>'; V.show($("bt-err"), e, () => backtest(rule)); }
  }
}

// ================= alerts summary =================
async function loadAlertSum() {
  let list = [];
  try { const r = await V.pick(() => V.call("GET", "/api/v1/alerts")); list = r.unavailable ? null : V.list(r.data, "alerts"); } catch (_) { list = null; }
  const rules = Watch.state?.rules || [];
  if (list === null) { $("alert-sum").innerHTML = '<div class="unavailable">Alert history is not available on this server version.</div>'; return; }
  const by = {}; for (const a of list) { const b = by[a.rule_id] || (by[a.rule_id] = { n: 0, open: 0 }); b.n++; if (a.decision === "unreviewed") b.open++; }
  Watch.fill($("alert-sum"), '<div class="asum">' + (rules.map((r) => { const b = by[r.id] || { n: 0, open: 0 }; return `<div class="ac${renab[r.id] ? "" : " off"}"><b>${esc(r.name)}</b><span class="mono">${b.open} to review · ${b.n} total</span><span class="quiet">${renab[r.id] ? "fires after " + r.hold + " frames" : "disabled"}</span></div>`; }).join("") || '<p class="quiet">No rules, so no alerts. Add a rule first.</p>') + "</div>");
}

// ================= unsaved changes + save =================
function toast(text) {
  const t = document.createElement("div"); t.className = "toast"; t.setAttribute("role", "status"); t.textContent = text; document.body.appendChild(t);
  setTimeout(() => { t.classList.add("out"); setTimeout(() => t.remove(), 300); }, 2600);
}
function touch() {
  syncPipe();
  const pk = pipeDirty(), mn = modDirty(), rn = ruleDirty(), total = pk.length + mn + rn, bad = pipeKeys().some((k) => validate(k));
  const bar = $("savebar"); bar.classList.toggle("show", total > 0); bar.setAttribute("aria-hidden", String(!total));
  $("sv-n").textContent = total + " unsaved change" + (total === 1 ? "" : "s");
  const parts = pk.slice(0, 4).concat(mn ? [mn + " model" + (mn > 1 ? "s" : "")] : [], rn ? [rn + " rule toggle" + (rn > 1 ? "s" : "")] : []);
  $("sv-keys").textContent = parts.join(" · ") + (pk.length > 4 ? " +" + (pk.length - 4) : "");
  $("sv-save").disabled = bad; $("sv-save").title = bad ? "Fix the highlighted fields first" : "";
  const nav = document.querySelectorAll(".su-nav .su-dot"); nav[0].hidden = !pk.length; nav[1].hidden = !mn; nav[2].hidden = !rn;
}
async function saveAll() {
  const btn = $("sv-save"); btn.disabled = true; V.clear($("sv-msg")); let ok = true;
  if (pipeDirty().length) {
    const body = {}; for (const k of pipeDirty()) body[k] = draft[k]; if (draft.mode !== saved.mode) body.mode = draft.mode;
    try { await V.call("POST", "/api/settings", body); Object.assign(saved, body); psig = ""; ferr = {}; }
    catch (e) { ok = false; const k = Object.keys(body).find((x) => String(e.message).includes(x)); if (k) { ferr[k] = e.message; document.querySelector(`.f[data-k="${k}"]`)?.scrollIntoView({ block: "center", behavior: Watch.reduced() ? "auto" : "smooth" }); } V.show($("sv-msg"), e); }
  }
  if (ok && modDirty()) {
    const mods = (Watch.state.modules || []).map((m, i) => ({ ...m, ...mdraft[i], labels: mdraft[i].labels }));
    try { await V.call("POST", "/api/modules", mods); Watch.state.modules = mods; mbase = mdraft.map(clone); msig = ""; }
    catch (e) { ok = false; V.show($("sv-msg"), e); }
  }
  if (ok && ruleDirty()) {
    const rules = curRules();
    try { await V.call("POST", "/api/rules", rules); Watch.state.rules = rules; rbase = { ...renab }; rsig = ""; }
    catch (e) { ok = false; V.show($("sv-msg"), e); }
  }
  if (ok) toast("Saved. Workers pick the changes up on their next reload.");
  touch(); btn.disabled = !!pipeKeys().some((k) => validate(k)) || (!ok && false);
}
$("sv-save").onclick = saveAll;
$("sv-discard").onclick = () => { draft = clone(saved); ferr = {}; mdraft = mbase.map(clone); renab = { ...rbase }; msig = ""; rsig = ""; ingest(Watch.state, true); V.clear($("sv-msg")); touch(); };
window.addEventListener("beforeunload", (e) => { if (pipeDirty().length || modDirty() || ruleDirty()) { e.preventDefault(); e.returnValue = ""; } });

// ================= state in =================
function ingest(s, force) {
  if (!s) return;
  const proc = Object.assign({}, DEFAULTS, s.process || { blur: s.blur }), p = sig(proc);
  if (!pipeDirty().length && (p !== psig || force)) {
    saved = proc; psig = p; draft = clone(saved);
    const ks = pipeKeys().join(","); if (ks !== keysSig) { keysSig = ks; buildPipe(); }
    syncPipe();
  }
  if (!modDirty() && sig(s.modules) !== msig) { msig = sig(s.modules); mdraft = (s.modules || []).map(pick); mbase = mdraft.map(clone); renderModels(); }
  if (!ruleDirty() && sig(s.rules) !== rsig) { rsig = sig(s.rules); rbase = Object.fromEntries((s.rules || []).map((r) => [r.id, !!r.enabled])); renab = { ...rbase }; renderRules(); loadAlertSum(); }
  $("n-models").textContent = (s.modules || []).filter((m) => m.enabled).length + "/" + (s.modules || []).length;
  $("n-rules").textContent = (s.rules || []).filter((r) => r.enabled).length + "/" + (s.rules || []).length;
  paintSaved(s); touch();
}

// ================= section nav =================
const links = [...document.querySelectorAll("#su-nav a")];
links.forEach((a) => a.addEventListener("click", (e) => { e.preventDefault(); const t = document.querySelector(a.getAttribute("href")); t.scrollIntoView({ behavior: Watch.reduced() ? "auto" : "smooth", block: "start" }); history.replaceState(null, "", a.getAttribute("href")); }));
if ("IntersectionObserver" in window) {
  const vis = new Map();
  const io = new IntersectionObserver((es) => {
    for (const e of es) vis.set(e.target.id, e.intersectionRatio > 0 ? e.boundingClientRect.top : null);
    const cur = [...vis].filter(([, t]) => t !== null).sort((a, b) => Math.abs(a[1]) - Math.abs(b[1]))[0];
    if (cur) links.forEach((a) => a.classList.toggle("on", a.getAttribute("href") === "#" + cur[0]));
  }, { rootMargin: "-20% 0px -60% 0px", threshold: [0, 1] });
  document.querySelectorAll(".su-sec").forEach((s) => io.observe(s));
}

ed = Watch.Editor.makeEditor($("ed-rule"), () => $("rule-submit").click());
ed.set(lineFor());
V.auth(); loadModels(); loadSources();
Watch.start((s) => { $("proc-live").textContent = s.process_line || ""; ingest(s); }, 1500);
