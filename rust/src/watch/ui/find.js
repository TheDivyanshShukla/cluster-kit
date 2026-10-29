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
let mode = "sql", api = 1, sources = [], models = [], sqlDirty = false, running = false, lastRun = null, savedKey = "", terms = [];
const GK = "watch.sourceGroups";
const groups = () => { try { return JSON.parse(localStorage.getItem(GK) || "{}") || {}; } catch (_) { return {}; } };
const CLASSIC = ["words", "template"];
const UNIT = { detections: "detection observations (not distinct people)", tracks: "distinct tracks", relations: "relation observations", frames: "frames", events: "events", aggregate: "aggregate result" };
const tbl = () => api ? "detections" : "sightings";
const q1 = (s) => "'" + String(s).replace(/'/g, "''") + "'";

// code editor now lives in app.js (Watch.Editor), shared with Rules & models
const { highlight, makeEditor } = Watch.Editor;

// Where did the server say the error is? Returns {start,end} char offsets or null.
function errorRange(msg, text) {
  if (!msg) return null;
  const lineStart = (n) => { let i = 0; for (let k = 1; k < n; k++) { const j = text.indexOf("\n", i); if (j < 0) return -1; i = j + 1; } return i; };
  const tokEnd = (i) => { const m = /^[A-Za-z0-9_.'"-]+|^\S/.exec(text.slice(i)); return i + (m ? m[0].length : 1); };
  let m = /lua:(\d+):/.exec(msg) || /line (\d+)(?:[, ]+col(?:umn)? (\d+))?/i.exec(msg) || /:(\d+):/.exec(msg);
  if (m) { const ls = lineStart(Number(m[1])); if (ls >= 0) { const le = text.indexOf("\n", ls), end = le < 0 ? text.length : le; if (m[2]) { const s = ls + Number(m[2]) - 1; return { start: s, end: Math.min(end, tokEnd(s)) }; } const lead = text.slice(ls, end).match(/^\s*/)[0].length; return { start: ls + lead, end }; } }
  m = /(?:position|pos|offset|at char(?:acter)?|at byte) (\d+)/i.exec(msg);
  if (m) { const s = Number(m[1]); return { start: s, end: tokEnd(s) }; }
  m = /near ['"`]?([^'"`\s]+)/i.exec(msg) || /unexpected ['"`]?([^'"`\s]+)/i.exec(msg) || /found ['"`]?([^'"`\s]+)/i.exec(msg);
  if (m) { const i = text.toLowerCase().indexOf(m[1].toLowerCase()); if (i >= 0) return { start: i, end: i + m[1].length }; }
  return null;
}
const SNIPS = {
  sql: [["Snippets…", ""], ["Phones", "SELECT * FROM {t}\nWHERE class = 'cell phone'\nLIMIT 50"], ["People or cars, confident", "SELECT * FROM {t}\nWHERE class IN ('person', 'car')\n  AND confidence >= 0.6\nLIMIT 100"], ["Count by camera (aggregation)", "SELECT COUNT(*) FROM {t}\nWHERE class = 'person'\nGROUP BY camera_id"], ["Phones near a person", "SELECT * FROM {t}\nWHERE class = 'cell phone' AND near_person = true\nLIMIT 50"]],
  lua: [["Snippets…", ""], ["Loop: people sharing a frame with a phone", "-- evidence rows: person detections in frames that also have a phone\nlocal phones = collect(db.detections{class = 'cell phone', min_confidence = 0.5})\nfor p in db.detections{class = 'person'} do\n  for _, ph in ipairs(phones) do\n    if ph.observation_id == p.observation_id then emit(p) emit(ph) end\n  end\nend"], ["Per-frame ratio (needs a Holding model)", "-- frames where >= 20% of people hold a phone\nfor f in db.frames{} do\n  local p, h = f.counts.person, f.relations['holding:cell phone']\n  if p and h and p > 0 and h / p >= 0.2 then emit(f) end\nend"], ["Distinct tracks, aggregate only", "-- person tracks longer than 10 s; returns values, no evidence windows\nlocal n = 0\nfor t in db.tracks{class = 'person'} do\n  if t.end_us - t.start_us > 10000000 then n = n + 1 emit(t) end\nend\nreturn {long_tracks = n}"], ["Count and distinct", "-- how many person detections, how many distinct tracks\nlocal rows = collect(db.detections{class = 'person', min_confidence = 0.5})\nfor _, r in ipairs(rows) do emit(r) end\nreturn {observations = count(rows), tracks = distinct(rows, 'track_id')}"]]
};
const edSql = makeEditor($("ed-sql"), () => runFind());
const edLua = makeEditor($("ed-lua"), () => runFind());
if (!edSql.get().trim()) edSql.set("SELECT * FROM detections WHERE class = 'person' AND confidence >= 0.5");
edLua.set("-- v1 Lua query: iterate db.detections / db.frames / db.tracks / db.relations, emit(row) for evidence\nlocal n = 0\nfor d in db.detections{class = 'person', min_confidence = 0.5} do\n  n = n + 1\n  emit(d)\nend\nreturn {person_detections = n}");
$("ed-sql").addEventListener("ed-input", () => { if (mode === "builder") { sqlDirty = true; paintSqlState(); } });

// ---------------------------------------------------------------- builder / state
function chosen() { return [...document.querySelectorAll("#b-sources input:checked")].map((i) => i.value); }
function timeUs() { const f = Watch.fromLocal($("from").value), t = Watch.fromLocal($("to").value); return { from: f ? Math.round(f * 1e6) : null, to: t ? Math.round(t * 1e6) : null, fromS: f, toS: t }; }
function genSql() {
  const w = [], cls = $("b-class").value.trim(), mc = $("b-conf").value.trim(), ids = chosen(), t = timeUs();
  if (cls) w.push("class = " + q1(cls));
  if (mc !== "" && Number.isFinite(Number(mc))) w.push("confidence >= " + Number(mc));
  if (!api && ids.length) w.push(ids.length === 1 ? "camera_id = " + q1(ids[0]) : "camera_id IN (" + ids.map(q1).join(", ") + ")");
  if (api) { if (t.from != null) w.push("capture_time_utc_us >= " + t.from); if (t.to != null) w.push("capture_time_utc_us < " + t.to); }
  else { if (t.fromS) w.push("ts >= " + Math.round(t.fromS)); if (t.toS) w.push("ts <= " + Math.round(t.toS)); }
  return `SELECT * FROM ${tbl()}` + (w.length ? "\nWHERE " + w.join("\n  AND ") : "") + "\nLIMIT 100";
}
function regen(force) {
  if (mode !== "builder") return;
  if (force) sqlDirty = false;
  if (!sqlDirty) edSql.set(genSql());
  paintSqlState();
}
function paintSqlState() {
  $("sql-regen").classList.toggle("hide", !(mode === "builder" && sqlDirty));
  $("sql-state").textContent = sqlDirty ? "edited by hand" : "generated";
  $("sql-help").textContent = api ? "Sources go in the query scope; the time range is in both the SQL and the scope." : "Sources and time are written into the SQL.";
}
function paintTime() {
  const hide = !api && (mode === "sql" || mode === "lua");
  $("time-block").classList.toggle("hide", hide);
  $("time-note").textContent = CLASSIC.includes(mode) || !api ? "Filters by indexing time (wall clock), not when it was filmed." : "Capture time (UTC) of the footage. Recordings without capture time cannot be time-filtered; use sources.";
  if (hide) $("time-note").textContent = "";
}
function paintHelp() {
  $("code-help").textContent = ({
    sql: "Counts are detection observations, not people visible at once. “Near” is proximity, not holding. Unsupported syntax returns an error at its position.",
    lua: api ? "Lua: for row in db.detections{class=…}, db.frames{}, db.relations{predicate=…}, db.tracks{}; emit(row) adds evidence; collect, count, distinct; return values. Sandboxed, 5 s budget. Errors mark their lua:<line>." : "Classic Lua only: return function(hit) … end with hit.class, .color, .camera, .at, .ts. v1 Lua needs the v1 server.",
    builder: "", words: "", template: ""
  })[mode] || "";
}
function setMode(m) {
  mode = m;
  document.querySelectorAll("#find-modes button").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.mode === m)));
  const sqlish = m === "sql" || m === "builder";
  $("mode-builder").classList.toggle("hide", m !== "builder");
  $("mode-sql").classList.toggle("hide", !sqlish);
  $("mode-lua").classList.toggle("hide", m !== "lua");
  $("mode-words").classList.toggle("hide", m !== "words");
  $("mode-template").classList.toggle("hide", m !== "template");
  $("classic-note").classList.toggle("hide", !CLASSIC.includes(m));
  const sn = SNIPS[m === "lua" ? "lua" : "sql"];
  $("run-classic").classList.toggle("hide", m !== "lua");
  $("snip").innerHTML = sn.map((s, i) => `<option value="${i}">${esc(s[0])}</option>`).join(""); $("snip").disabled = !(sqlish || m === "lua");
  paintTime(); paintHelp(); regen(); paintSqlState();
  V.clear($("find-err")); edSql.setError(null); edLua.setError(null);
}
$("snip").onchange = () => {
  const m = mode === "lua" ? "lua" : "sql", s = SNIPS[m][Number($("snip").value)]; if (!s || !s[1]) return;
  const body = s[1].replace(/\{t\}/g, tbl());
  (m === "lua" ? edLua : edSql).set(body); if (mode === "builder") { sqlDirty = true; paintSqlState(); }
  $("snip").value = "0";
};
function askBody() {
  return { mode, words: mode === "sql" || mode === "builder" ? edSql.get() : $("q").value, class: $("t-class").value, color: $("t-color").value, camera: mode === "template" ? $("t-cam").value : "", from: Watch.fromLocal($("from").value), to: Watch.fromLocal($("to").value), lua: edLua.get() };
}

// ---------------------------------------------------------------- refs
function paintSources() {
  const sel = new Set(chosen());
  const want = new URLSearchParams(location.search).get("sources");
  if (want && !paintSources.done) want.split(",").forEach((s) => sel.add(s));
  paintSources.done = true;
  $("b-sources").innerHTML = sources.length ? sources.map((s) => `<label><input type="checkbox" value="${esc(s.id)}" ${sel.has(s.id) ? "checked" : ""}> ${esc(s.name || s.id)}</label>`).join("") : '<span class="quiet">No sources. <a href="/sources">Add one</a>.</span>';
  const g = groups();
  $("b-group").innerHTML = '<option value="">No group</option>' + Object.keys(g).map((n) => `<option value="${esc(n)}">${esc(n)} (${g[n].length})</option>`).join("");
  regen();
}
async function loadRefs() {
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/sources"), () => V.call("GET", "/api/sources"));
    api = r.v;
    sources = api ? V.list(r.data, "sources") : (V.list(r.data, "expanded").length ? V.list(r.data, "expanded") : V.list(r.data, "sources"));
  } catch (e) { V.show($("find-err"), e, loadRefs); }
  try {
    const r = await V.pick(() => V.call("GET", "/api/v1/models"), () => V.call("GET", "/api/capabilities"));
    models = r.v ? V.modelsOf(r.data) : V.list(r.data, "modules");
    if (r.v) V.list(r.data, "models").forEach((e) => { if (e.module && !e.model) models.push({ labels: e.module.labels || [], supported_relations: [] }); });
  } catch (_) {}
  $("class-list").innerHTML = [...new Set(models.flatMap((m) => m.labels || []))].sort().map((c) => `<option value="${esc(c)}">`).join("");
  paintSources(); setMode(mode);
  if (!edSql.get()) edSql.set(`SELECT * FROM ${tbl()}\nWHERE class = 'person'\nLIMIT 100`);
}
$("b-sources").onchange = () => regen();
$("b-group").onchange = () => { const ids = groups()[$("b-group").value] || []; document.querySelectorAll("#b-sources input").forEach((i) => { i.checked = ids.includes(i.value); }); regen(); };
["b-class", "b-conf", "from", "to"].forEach((id) => $(id).addEventListener("input", () => regen()));
$("sql-regen").onclick = () => regen(true);
$("find-modes").onclick = (e) => { const b = e.target.closest("button"); if (b) setMode(b.dataset.mode); };
function setPane(p) { $("ask").dataset.pane = p; document.querySelectorAll("#ws-tabs button").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.pane === p))); }
$("ws-tabs").onclick = (e) => { const b = e.target.closest("button"); if (b) setPane(b.dataset.pane); };

// ---------------------------------------------------------------- results
const mk = (text) => { const t = String(text == null ? "" : text); if (!terms.length) return esc(t); const rx = new RegExp("(" + terms.map((x) => x.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|") + ")", "ig"); return t.split(rx).map((p, i) => i % 2 ? `<mark>${esc(p)}</mark>` : esc(p)).join(""); };
function termsFrom(sql) {
  const out = new Set();
  (sql || "").replace(/class\s*(?:=|in|like)\s*\(?([^)\n]*?)(?:\)|\band\b|\bor\b|\blimit\b|\bgroup\b|$)/ig, (_, g) => { (g.match(/'((?:[^']|'')*)'/g) || []).forEach((s) => out.add(s.slice(1, -1).replace(/''/g, "'").replace(/%/g, ""))); });
  (sql || "").replace(/(?:class|camera|camera_id)\s*=\s*'([^']*)'/ig, (_, g) => out.add(g));
  return [...out].filter(Boolean);
}
function setBusy(b) { running = b; $("run-find").disabled = b; $("run-find").firstChild.textContent = b ? "Running… " : "Run "; }
function clipHref(h) {
  const q = new URLSearchParams(); q.set("id", h.camera_id || ""); q.set("at", String(h.at || 0));
  const f = Watch.fromLocal($("from").value), t = Watch.fromLocal($("to").value);
  if (f) q.set("from", String(f)); if (t) q.set("to", String(t));
  return "/clip?" + q.toString();
}
function badge(n) { $("res-badge").textContent = n == null ? "" : n; }
function showHits(hits) {
  const list = Watch.fold(hits || []);
  $("hit-count").textContent = list.length ? list.length + " moments (classic index)" : "No moments. Only indexed frames are searchable.";
  badge(list.length);
  $("hits").innerHTML = list.length ? `<div class="scroll-x"><table><thead><tr><th></th><th>class</th><th>camera</th><th>indexed</th><th>at</th><th>n</th><th></th></tr></thead><tbody>${list.map((h) => `<tr><td><img class="th" alt="" loading="lazy" src="/api/hit/${h.id}"></td><td>${mk([h.color, h.class].filter(Boolean).join(" "))}</td><td>${mk(h.camera)}</td><td>${esc(Watch.when(h.ts))}</td><td>${esc(Watch.clipTime(h.at))}</td><td>${h.n}</td><td><a class="btn sm" href="${esc(clipHref(h))}">Open</a></td></tr>`).join("")}</tbody></table></div>` : '<div class="empty-state"><h2>No matches</h2><p>No match is not evidence of absence: only frames already indexed by an installed model are searched.</p></div>';
}
function showLegacySql(j) {
  const result = j.result || j;
  if (!result.aggregate && (j.rows || j.hits)) { showHits(j.rows || j.hits); return; }
  const rows = result.groups || j.rows || [], cols = j.columns || (rows.length ? Object.keys(rows[0]) : []);
  $("hit-count").textContent = result.count != null ? `${result.count} indexed detections (classic)` : `${rows.length} rows (classic)`; badge(result.count != null ? result.count : rows.length);
  if (typeof result.count === "number" && !rows.length) { $("hits").innerHTML = `<div class="empty-state"><h2>${result.count.toLocaleString()} detections</h2><p>Indexed detection count, not a simultaneous headcount.</p></div>`; return; }
  $("hits").innerHTML = rows.length ? `<div class="scroll-x"><table><thead><tr>${cols.map((c) => `<th>${esc(c)}</th>`).join("")}</tr></thead><tbody>${rows.map((row) => `<tr>${cols.map((c, i) => `<td>${mk(Array.isArray(row) ? row[i] : row[c])}</td>`).join("")}</tr>`).join("")}</tbody></table></div>` : '<div class="empty-state"><h2>No matching sightings</h2><p>No match is not evidence that nothing was there.</p></div>';
}
function holdingUnsupported() { return !models.some((m) => (m.supported_relations || []).includes("holding")); }
function paintRun(run, matches, next, extra) {
  extra = extra || {};
  lastRun = run;
  const c = run.coverage || {}, unit = extra.unit || UNIT[run.kind] || run.kind, warns = (run.warnings || []).slice();
  if (/holding/i.test(run.normalized || edSql.get()) && holdingUnsupported() && !warns.some((w) => /holding/i.test(w))) warns.push("holding is unsupported by the installed models; “near” is only a proximity heuristic.");
  if (c.failed > 0) warns.push(`${V.n(c.failed)} frames failed analysis and are not covered.`);
  if (c.skipped > 0) warns.push(`${V.n(c.skipped)} frames were skipped by sampling gates and cannot match.`);
  if (run.result_count === 0) warns.push("Zero results is not evidence of absence: only analyzed intervals were searched; classes no installed model detects are unknown, not zero.");
  $("run-summary").innerHTML = `<div class="runbox"><div class="runhead"><b class="big mono">${V.n(run.result_count)}</b> <span>${esc(unit)}</span> <span class="spacer"></span><a class="btn primary sm" id="open-review" href="/review?run=${encodeURIComponent(run.id)}">Open in review</a><button type="button" class="sm" id="copy-run">Copy link</button></div>
    <div class="kv"><span>decoded <b>${V.n(c.decoded)}</b></span><span>inferred <b>${V.n(c.inferred)}</b></span><span>skipped <b>${V.n(c.skipped)}</b></span><span>failed <b>${V.n(c.failed)}</b></span><span>analyzed <b>${(V.analyzed(c) / 1e6).toFixed(1)}s</b></span><span><b>${((run.elapsed_us || 0) / 1000).toFixed(1)}</b> ms</span><span>run <b>${esc(run.id)}</b></span></div>
    ${extra.values != null && !(typeof extra.values === "object" && !Object.keys(extra.values).length) ? `<div class="terms"><span class="quiet">values</span> <pre class="vals mono">${esc(JSON.stringify(extra.values, null, 2))}</pre></div>` : ""}
    ${terms.length ? `<div class="terms quiet">matching: ${terms.map((t) => `<mark>${esc(t)}</mark>`).join(" ")}</div>` : ""}
    ${warns.length ? `<ul class="warnings">${warns.map((w) => `<li>${esc(w)}</li>`).join("")}</ul>` : ""}<div class="msg" id="run-msg"></div></div>`;
  $("hit-count").textContent = matches.length ? `${matches.length} of ${V.n(run.result_count)} shown` : ""; badge(V.n(run.result_count));
  $("hits").innerHTML = '<div class="scroll-x"><table id="mt"><thead><tr><th></th><th>source</th><th>time</th><th>tracks</th><th>conf</th><th></th></tr></thead><tbody></tbody></table></div>'; addMatches(matches);
  $("more").classList.toggle("hide", !next); $("more").dataset.cursor = next || "";
  $("copy-run").onclick = async () => { try { await navigator.clipboard.writeText(location.origin + "/review?run=" + run.id); V.ok($("run-msg"), "Link copied."); } catch (_) { V.show($("run-msg"), { message: "Could not copy; use Open in review." }); } };
}
// Real failure only: say why (the server's message) instead of a bare placeholder.
window.thumbFail = (img) => {
  const sp = Object.assign(document.createElement("span"), { className: "th noimg", textContent: "n/a", title: "frame unavailable" });
  img.replaceWith(sp);
  fetch(img.src).then((r) => r.json()).then((j) => { if (j && j.error) sp.title = j.error.message; }).catch(() => {});
};
function addMatches(ms) {
  const tb = $("mt") && $("mt").tBodies[0]; if (!tb) return;
  tb.insertAdjacentHTML("beforeend", ms.map((m) => `<tr><td><img class="th" alt="" loading="lazy" src="/api/v1/media/${encodeURIComponent(m.media_id)}/frame?t_us=${m.start_us}" onerror="thumbFail(this)"></td><td>${mk(m.source_id)}</td><td title="${m.capture_start_utc_us != null ? "" : "media time; no capture time recorded"}">${m.still ? "still" : V.usTime(m.start_us) + "–" + V.usTime(m.end_us)}${m.capture_start_utc_us != null ? "<br>" + esc(V.utcTime(m.capture_start_utc_us + m.start_us)) : ""}</td><td>${m.distinct_tracks || ""}</td><td>${Watch.num(m.max_confidence)}</td><td><a class="btn sm" href="/review?run=${encodeURIComponent(m.query_run_id)}&match=${encodeURIComponent(m.id)}">Review</a></td></tr>`).join(""));
}
$("more").onclick = async () => {
  if (!lastRun) return; $("more").disabled = true; V.clear($("more-msg"));
  try {
    const j = await V.call("GET", `/api/v1/queries/${encodeURIComponent(lastRun.id)}/matches?limit=50&cursor=${encodeURIComponent($("more").dataset.cursor)}`);
    addMatches(V.list(j, "matches")); const next = j && j.next; $("more").dataset.cursor = next || ""; $("more").classList.toggle("hide", !next);
  } catch (e) { V.show($("more-msg"), e); } finally { $("more").disabled = false; }
};
async function runV1(extra) {
  const t = timeUs(), ids = chosen();
  const body = Object.assign({}, extra, { scope: { source_ids: ids, media_ids: [], from_utc_us: t.from, to_utc_us: t.to } });
  const j = await V.call("POST", "/api/v1/queries", body);
  if (!j || !j.run) throw { message: "The server returned no query run." };
  paintRun(j.run, V.list(j, "matches"), j.next, { values: j.values, unit: j.unit });
}
async function runFind() {
  if (running) return;
  V.clear($("find-err")); edSql.setError(null); edLua.setError(null);
  $("run-summary").innerHTML = ""; $("hits").innerHTML = ""; $("more").classList.add("hide"); lastRun = null; badge(null);
  const sqlish = mode === "sql" || mode === "builder", isLua = mode === "lua", ed = isLua ? edLua : edSql;
  if ((sqlish || isLua) && !ed.get().trim()) { V.show($("find-err"), { message: isLua ? "Write a Lua script first." : "Enter a SQL query first." }); return; }
  terms = sqlish ? termsFrom(edSql.get()) : mode === "words" ? $("q").value.split(/[\s,]+/).filter(Boolean) : mode === "template" ? [$("t-class").value, $("t-color").value, $("t-cam").value].filter(Boolean) : [];
  setBusy(true); $("hit-count").textContent = "Running…"; setPane("results");
  try {
    if ((sqlish || isLua) && api) {
      try { await runV1(isLua ? { lua: edLua.get() } : { sql: edSql.get().trim() }); return; }
      catch (e) { if (!V.absent(e)) throw e; api = 0; await loadRefs(); if (mode === "builder") regen(true); }
    }
    if (sqlish) showLegacySql(await V.call("POST", "/api/query", { sql: edSql.get() }));
    else { const j = await V.call("POST", "/api/find", askBody()); if (j.error) throw { message: j.error }; showHits(j.hits); }
  } catch (err) {
    $("hit-count").textContent = "";
    V.show($("find-err"), err, runFind);
    if (sqlish || isLua) { const r = errorRange(err.message, ed.get()); if (r) ed.setError(r); setPane("code"); }
  } finally { setBusy(false); }
}
$("run-classic").onclick = async () => {
  V.clear($("find-err")); edLua.setError(null); setBusy(true); setPane("results"); terms = [];
  try { const j = await V.call("POST", "/api/find", { mode: "lua", lua: edLua.get(), from: Watch.fromLocal($("from").value), to: Watch.fromLocal($("to").value) }); if (j.error) throw { message: j.error }; $("run-summary").innerHTML = ""; showHits(j.hits); }
  catch (e) { V.show($("find-err"), e); setPane("code"); } finally { setBusy(false); }
};
$("ask").onsubmit = (e) => { e.preventDefault(); runFind(); };

// ---------------------------------------------------------------- saved searches (legacy /api/searches; no v1 route)
function paintSaved(list) {
  $("saved").innerHTML = list.map((s, i) => `<div class="saved-row"><button type="button" data-saved="${i}"><span class="when">${esc(s.mode || "words")}</span> <b>${esc(s.name)}</b><br><span class="quiet">${esc((s.words || s.class || s.lua || "").replace(/\s+/g, " ")).slice(0, 60)}</span></button><button type="button" class="sm" data-drop="${i}" aria-label="Remove saved search ${esc(s.name)}">×</button></div>`).join("") || '<p class="quiet">None yet.</p>';
}
async function putSaved(list, okText) {
  try { await V.call("POST", "/api/searches", list); Watch.state = Object.assign(Watch.state || {}, { searches: list }); savedKey = JSON.stringify(list); paintSaved(list); V.ok($("save-msg"), okText); }
  catch (e) { V.show($("save-msg"), e); }
}
$("saved").onclick = async (e) => {
  const t = e.target.closest("button"); if (!t) return;
  const list = ((Watch.state && Watch.state.searches) || []).slice();
  if (t.dataset.drop != null) { const s = list.splice(Number(t.dataset.drop), 1)[0]; await putSaved(list, `Removed “${s ? s.name : "search"}”.`); return; }
  const s = list[Number(t.dataset.saved)]; if (!s) return;
  V.clear($("save-msg"));
  const m = s.mode === "builder" ? "sql" : (s.mode || "words");
  setMode(m);
  if (m === "sql") edSql.set(s.words || ""); else if (m !== "lua") $("q").value = s.words || "";
  $("t-class").value = s.class || ""; $("t-color").value = s.color || ""; $("t-cam").value = s.camera || "";
  $("from").value = Watch.toLocal(s.from); $("to").value = Watch.toLocal(s.to);
  if (s.lua) edLua.set(s.lua);
  runFind();
};
$("save-search").onclick = async () => {
  const name = $("search-name").value.trim();
  if (!name) { V.show($("save-msg"), { message: "Name the search before saving it." }); $("search-name").focus(); return; }
  const list = ((Watch.state && Watch.state.searches) || []).slice();
  let id = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "search";
  while (list.some((s) => s.id === id)) id += "-2";
  const body = askBody(); if (mode === "builder") body.mode = "sql";
  await putSaved(list.concat([{ id, name, ...body }]), `Saved “${name}”. Stores the query and time range, not the source scope.`);
  $("search-name").value = "";
};

Watch.start((s) => { const key = JSON.stringify(s.searches || []); if (key !== savedKey) { savedKey = key; paintSaved(s.searches || []); } }, 1500);
V.auth(); setMode("sql"); loadRefs();
