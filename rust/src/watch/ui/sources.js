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
let sources = [], api = 1, jobs = [], jobsUnavailable = false, media = {}, filter = "", editing = null, saving = false;
const selected = new Set();
const GK = "watch.sourceGroups";
const KIND = { live: "Live camera", webcam: "Webcam", archive: "Recording", file: "Looping video", image: "Image", folder: "Image folder" };
const hints = {
  live: ["Stream address", "rtsp://192.168.1.20:554/stream", "Use an RTSP or HTTP address reachable from the assigned worker."],
  webcam: ["Device number", "0", "The webcam must be attached to the assigned worker. Start with device 0."],
  archive: ["Video path", "vision/footage/classroom.mp4", "Indexes the recording once. Use a path accessible from every worker that may process it."],
  image: ["Image path", "vision/footage/classroom.jpg", "Indexes a single image. The path is on the engine computer, not this browser."],
  folder: ["Folder path", "vision/footage/classroom-images", "Indexes supported images from a folder accessible to the worker."],
  file: ["Video path", "vision/footage/classroom.mp4", "Loops the recording for live demonstrations. Use Recorded video for historical indexing."]
};
function groups() { try { return JSON.parse(localStorage.getItem(GK) || "{}") || {}; } catch (_) { return {}; } }
function putGroups(g) { try { localStorage.setItem(GK, JSON.stringify(g)); return true; } catch (_) { return false; } }
const norm = (s) => ({ id: s.id, name: s.name || s.id, kind: s.kind === "collection" ? "folder" : s.kind, location: s.location != null ? s.location : s.url, credential_ref: s.credential_ref || null, enabled: s.enabled !== false, tz: s.capture_timezone || "", raw: s });

function srcMedia(id) { const m = Object.values(media).filter((x) => x && x.source_id === id); return m; }
function mediaLine(s) {
  const m = srcMedia(s.id);
  if (!m.length) return api ? `<span class="quiet">Recording availability: no indexed media reported yet.</span>` : `<span class="quiet">Recording availability is not reported by this server version.</span>`;
  const c = { available: 0, missing: 0, expired: 0, unknown: 0 };
  m.forEach((x) => { c[x.availability] = (c[x.availability] || 0) + 1; });
  return `<span class="badge ok">${c.available} available</span> ` + (c.missing ? `<span class="badge bad" title="The file is no longer at its recorded path. Existing detections stay searchable but playback is unavailable.">${c.missing} missing</span> ` : "") + (c.expired ? `<span class="badge warn" title="Removed by the retention policy.">${c.expired} expired (retention)</span> ` : "") + (c.unknown ? `<span class="badge">${c.unknown} unknown</span>` : "");
}
function legacyStatus(s) {
  const c = ((Watch.state && Watch.state.cameras) || []).find((x) => x.id === s.id);
  if (!c) return '<span class="quiet">Not reported by a worker yet.</span>';
  return `<span class="badge ${c.completed ? "ok" : "pop"}">${esc(c.completed ? "Indexing complete" : (c.status || "running"))}</span> <span class="quiet">${Number(c.indexed_frames || 0).toLocaleString()} frames indexed${c.feed ? " · " + esc(c.feed) : ""}</span>`;
}
function render() {
  const q = filter;
  const shown = sources.filter((s) => [s.name, s.location, s.id].join(" ").toLowerCase().includes(q));
  $("source-count").textContent = sources.length + (sources.length === 1 ? " source" : " sources");
  $("source-list").innerHTML = shown.length ? shown.map((s) => `<article class="src-row">
    <input type="checkbox" data-sel="${esc(s.id)}" ${selected.has(s.id) ? "checked" : ""} aria-label="Select ${esc(s.name)} for a group">
    <div><span class="source-kind">${esc(KIND[s.kind] || s.kind || "Source")}${s.enabled ? "" : ' <span class="badge warn">disabled</span>'}</span><h3>${esc(s.name)}</h3>
      <p title="${esc(V.redact(s.location))}">${esc(V.redact(s.location))}</p>
      ${s.credential_ref ? '<p><span class="badge">credential stored as reference</span></p>' : ""}
      <p>${api ? mediaLine(s) : legacyStatus(s)}</p></div>
    <div class="source-actions"><a class="save" href="${api ? "/find?sources=" + encodeURIComponent(s.id) : "/clip?id=" + encodeURIComponent(s.id)}">${api ? "Search" : "Review"}</a><button type="button" data-edit="${esc(s.id)}">Edit</button>${api ? `<button type="button" data-toggle="${esc(s.id)}">${s.enabled ? "Disable" : "Enable"}</button>` : `<button type="button" data-remove="${esc(s.id)}">Remove</button>`}</div></article>`).join("")
    : `<div class="empty-state"><h2>${q ? "No matching sources" : "Build your source library"}</h2><p>${q ? "Try another source name or clear the filter." : "Add a camera, recording, or image folder. The engine will assign it to an available worker."}</p></div>`;
  paintGroups();
}
async function loadSources() {
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/sources"), () => V.call("GET", "/api/sources"));
    api = r.v;
    if (api) sources = V.list(r.data, "sources").map(norm);
    else { const full = V.list(r.data, "expanded"); sources = (full.length ? full : V.list(r.data, "sources")).map(norm); }
    $("opt-file").hidden = !!api; $("opt-file").disabled = !!api;
    $("cred-wrap").hidden = !api; $("tz-wrap").hidden = !api;
    V.clear($("source-error"));
    render(); loadJobs();
  } catch (e) { V.show($("source-feedback"), e, loadSources); $("source-list").innerHTML = ""; $("source-count").textContent = "Unavailable"; }
}

// ---- jobs / coverage ----
let pollTimer = 0;
async function loadJobs() {
  clearTimeout(pollTimer);
  if (!api) { jobsUnavailable = true; paintJobs(); return; }
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/jobs"));
    if (r.unavailable) { jobsUnavailable = true; paintJobs(); return; }
    jobsUnavailable = false;
    jobs = V.list(r.data, "jobs");
    V.clear($("jobs-msg"));
    const need = [...new Set(jobs.flatMap((j) => j.media_ids || []))].filter((id) => !media[id]).slice(0, 40);
    await Promise.all(need.map(async (id) => { try { media[id] = await V.call("GET", "/api/v1/media/" + encodeURIComponent(id)); } catch (e) { if (!V.absent(e)) media[id] = null; } }));
    // Refresh availability for assets that were not yet available (a file can go missing or return).
    paintJobs(); render();
  } catch (e) { V.show($("jobs-msg"), e, loadJobs); }
  const live = jobs.some((j) => j.state === "queued" || j.state === "running");
  pollTimer = setTimeout(loadJobs, live ? 2000 : 8000);
}
const STATE = { queued: "warn", running: "pop", completed: "ok", failed: "bad", cancelled: "warn" };
function paintJobs() {
  const box = $("jobs");
  if (jobsUnavailable) {
    const list = sources.filter((s) => s.kind !== "live" || true);
    box.innerHTML = `<div class="unavailable">Index jobs are not available on this server version. Per-source status from the workers:</div>` + (list.map((s) => `<div class="coverage-row"><b>${esc(s.name)}</b>${legacyStatus(s)}</div>`).join("") || '<p class="quiet">No sources.</p>');
    return;
  }
  if (!jobs.length) { box.innerHTML = '<div class="empty-state"><h2>No index jobs yet</h2><p>Adding a recording, image or folder starts an import job. Progress and coverage appear here.</p></div>'; return; }
  box.innerHTML = jobs.slice().sort((a, b) => (b.created_utc_us || 0) - (a.created_utc_us || 0)).map((j) => {
    const c = j.coverage || {}, ms = (j.media_ids || []).map((id) => media[id]).filter(Boolean);
    const total = ms.length === (j.media_ids || []).length ? ms.reduce((a, m) => a + (m.duration_us || 0), 0) : 0;
    const done = Object.values(j.checkpoint || {}).reduce((a, b) => a + b, 0);
    const pct = j.state === "completed" ? 100 : (total ? Math.min(100, Math.round(done / total * 100)) : null);
    const names = [...new Set(ms.map((m) => (sources.find((s) => s.id === m.source_id) || {}).name || m.source_id))].join(", ");
    const missing = ms.filter((m) => m.availability !== "available");
    return `<div class="card"><h3>${esc(names || j.id)} <span class="badge ${STATE[j.state] || ""}">${esc(j.state)}</span></h3>
      <div class="kv"><span>decoded <b>${V.n(c.decoded)}</b></span><span>inferred <b>${V.n(c.inferred)}</b></span><span>skipped <b>${V.n(c.skipped)}</b></span><span>failed <b ${c.failed ? 'style="color:#E5846B"' : ""}>${V.n(c.failed)}</b></span><span>analyzed <b>${(V.analyzed(c) / 1e6).toFixed(1)} s</b> in ${(c.analyzed || []).length} interval${(c.analyzed || []).length === 1 ? "" : "s"}</span></div>
      ${pct != null ? `<div class="progress" role="progressbar" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100"><i style="width:${pct}%"></i></div>` : ""}
      <p class="quiet">${pct != null ? pct + "% of media time" : "Total length not known yet"} · ${(j.media_ids || []).length} media item${(j.media_ids || []).length === 1 ? "" : "s"} · job ${esc(j.id)}</p>
      ${j.state === "completed" && c.decoded ? `<p class="quiet">${V.n(c.inferred)} of ${V.n(c.decoded)} decoded frames were inferred; skipped frames cannot match a search.</p>` : ""}
      ${missing.length ? `<p class="msg bad">${missing.length} media item${missing.length === 1 ? " is" : "s are"} ${esc([...new Set(missing.map((m) => m.availability))].join("/"))}: playback is unavailable, existing detections stay searchable.</p>` : ""}
      ${j.error ? `<p class="msg bad">${esc(j.error.message)}${j.error.retryable ? " (retryable)" : ""}</p>` : ""}</div>`;
  }).join("");
}

// ---- add / edit ----
function formReset() {
  editing = null; $("source-form").reset(); $("s-kind").onchange();
  $("form-title").textContent = "Add a source"; $("source-add").textContent = "Add source"; $("source-cancel").hidden = true;
  $("cred-help").textContent = "Write-only. It is sent once and stored as a reference; it is never shown again.";
}
$("s-kind").onchange = () => { const [label, ph, help] = hints[$("s-kind").value]; $("s-url-label").textContent = label; $("s-url").placeholder = ph; $("s-url-help").textContent = help; };
$("source-filter").oninput = (e) => { filter = e.target.value.toLowerCase(); render(); };
$("source-cancel").onclick = formReset;
$("source-form").onsubmit = async (e) => {
  e.preventDefault(); if (saving) return;
  const name = $("s-name").value.trim(); let url = $("s-url").value.trim(), cred = $("s-cred").value;
  if (!name || !url) { V.show($("source-error"), { message: "Enter a name and an address." }); return; }
  const kind = $("s-kind").value, old = editing && sources.find((s) => s.id === editing);
  const id = editing || "src-" + Date.now().toString(36);
  saving = true; $("source-add").disabled = true; $("source-add").textContent = editing ? "Saving…" : "Adding…"; V.clear($("source-error"));
  try {
    if (api) {
      const m = url.match(/^([a-z][a-z0-9+.-]*:\/\/)([^\/@\s]+)@(.*)$/i);
      if (m) { if (!cred) cred = decodeURIComponent(m[2]); url = m[1] + m[3]; }
      const body = { id, name, kind: kind === "folder" ? "collection" : kind, location: url, credential_ref: old ? old.credential_ref : null, eligible_workers: old ? (old.raw.eligible_workers || []) : [], enabled: old ? old.enabled : true, capture_timezone: $("s-tz").value.trim() || null, import: !editing && ["archive", "image", "folder"].includes(kind) };
      if (cred) body.credential = cred;
      const r = await V.call("POST", "/api/v1/sources", body);
      const job = r && (r.job || r.import_job);
      await loadSources();
      V.ok($("source-feedback"), (editing ? "Saved " : "Added ") + name + "." + (cred ? " Credential stored as reference." : "") + (job ? " Import job " + (job.id || job) + " started." : ""));
    } else {
      const base = sources.map((s) => s.raw);
      const rec = Object.assign({}, old ? old.raw : { start: 0, zones: [] }, { id, name, url, kind });
      const next = old ? base.map((s) => s.id === id ? rec : s) : base.concat([rec]);
      await V.call("POST", "/api/sources", next);
      await loadSources();
      V.ok($("source-feedback"), (editing ? "Saved " : "Added ") + name + ". Waiting for its assigned worker to report.");
    }
    formReset();
  } catch (err) { V.show($("source-error"), err); }
  finally { saving = false; $("source-add").disabled = false; $("source-add").textContent = editing ? "Save changes" : "Add source"; }
};
$("source-list").onclick = async (e) => {
  const t = e.target;
  if (t.dataset.edit) {
    const s = sources.find((x) => x.id === t.dataset.edit); if (!s) return;
    editing = s.id; $("s-name").value = s.name; $("s-kind").value = s.kind === "collection" ? "folder" : s.kind; $("s-kind").onchange();
    $("s-url").value = s.location || ""; $("s-cred").value = ""; $("s-tz").value = s.tz;
    $("form-title").textContent = "Edit source"; $("source-add").textContent = "Save changes"; $("source-cancel").hidden = false;
    $("cred-help").textContent = s.credential_ref ? "A credential is stored as a reference. Leave blank to keep it, or enter a new one to replace it." : "Write-only. It is sent once and stored as a reference; it is never shown again.";
    $("s-name").focus();
  } else if (t.dataset.toggle) {
    const s = sources.find((x) => x.id === t.dataset.toggle); if (!s) return;
    t.disabled = true;
    try { await V.call("POST", "/api/v1/sources", Object.assign({}, s.raw, { enabled: !s.enabled })); await loadSources(); V.ok($("source-feedback"), (s.enabled ? "Disabled " : "Enabled ") + s.name + "."); }
    catch (err) { V.show($("source-feedback"), err); t.disabled = false; }
  } else if (t.dataset.remove) {
    const s = sources.find((x) => x.id === t.dataset.remove); if (!s) return;
    if (!confirm(`Remove ${s.name}? This stops indexing the source. Existing sightings remain in the index.`)) return;
    t.disabled = true;
    try { await V.call("POST", "/api/sources", sources.filter((x) => x.id !== s.id).map((x) => x.raw)); selected.delete(s.id); await loadSources(); V.ok($("source-feedback"), "Removed " + s.name + "."); }
    catch (err) { V.show($("source-feedback"), err); t.disabled = false; }
  } else if (t.dataset.sel) {
    t.checked ? selected.add(t.dataset.sel) : selected.delete(t.dataset.sel); paintGroups();
  }
};

// ---- groups (localStorage, this browser only) ----
function paintGroups() {
  const g = groups(), names = Object.keys(g);
  $("group-list").innerHTML = names.map((n) => `<span class="chip"><button type="button" data-apply="${esc(n)}" title="Select this group's sources">${esc(n)} (${g[n].length})</button><button type="button" data-drop="${esc(n)}" aria-label="Delete group ${esc(n)}">×</button></span>`).join("") || '<span class="quiet">No groups saved in this browser.</span>';
  const a = $("group-search"); a.hidden = !selected.size; a.href = "/find?sources=" + encodeURIComponent([...selected].join(","));
  a.textContent = `Search ${selected.size} selected`;
}
$("group-save").onclick = () => {
  const n = $("group-name").value.trim();
  if (!n) { V.show($("group-msg"), { message: "Name the group first." }); return; }
  if (!selected.size) { V.show($("group-msg"), { message: "Tick at least one source first." }); return; }
  const g = groups(); g[n] = [...selected];
  if (putGroups(g)) { V.ok($("group-msg"), `Saved group “${n}” in this browser.`); $("group-name").value = ""; paintGroups(); }
  else V.show($("group-msg"), { message: "This browser blocked local storage, so the group could not be saved." });
};
$("group-list").onclick = (e) => {
  const t = e.target, g = groups();
  if (t.dataset.apply && g[t.dataset.apply]) { selected.clear(); g[t.dataset.apply].forEach((id) => sources.some((s) => s.id === id) && selected.add(id)); render(); V.ok($("group-msg"), `Selected ${selected.size} source${selected.size === 1 ? "" : "s"} from “${t.dataset.apply}”.`); }
  else if (t.dataset.drop) { delete g[t.dataset.drop]; putGroups(g); paintGroups(); V.ok($("group-msg"), "Group deleted."); }
};

// ---- model coverage ----
async function coverage() {
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/models"), () => V.call("GET", "/api/capabilities"));
    if (r.v) {
      const ms = V.modelsOf(r.data);
      $("capabilities").innerHTML = ms.map((m) => `<div class="coverage-row"><b>${esc(m.id)} <span class="badge">${esc(m.version || "")}</span></b><span>${esc(m.provider)} · validation: ${esc(m.validation)}</span><p>${esc((m.labels || []).join(", ") || "No classes declared")}</p></div>`).join("") || '<p class="quiet">No models installed.</p>';
    } else {
      const ms = V.list(r.data, "modules");
      $("capabilities").innerHTML = ms.map((m) => `<div class="coverage-row"><b>${esc(m.name || m.id)}</b><span>${m.enabled ? (m.available ? "Enabled · installed" : "Model not installed") : "Disabled"}</span><p>${esc((m.labels || []).join(", ") || "No object classes declared")}</p></div>`).join("") || '<p class="quiet">No detection modules configured.</p>';
    }
  } catch { $("capabilities").innerHTML = '<p class="quiet">Model availability could not be checked. Open Rules & models to inspect the configuration.</p>'; }
}
V.auth();
Watch.start(() => { if (!api && !editing) paintJobs(); }, 3000);
loadSources(); coverage();
$("s-kind").onchange();
window.addEventListener("pagehide", () => clearTimeout(pollTimer));
