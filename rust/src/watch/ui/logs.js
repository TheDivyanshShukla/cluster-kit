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
let tab = "audit", cat = "", api = null, rows = [], cursor = null, note = "", gen = 0;
const setNote = () => { $("index-note").textContent = note; };

function auditRow(x) {
  const t = x.utc_us != null ? new Date(x.utc_us / 1000) : (x.ts ? new Date(x.ts * 1000) : null);
  // "unknown" (a rule that cannot be evaluated) and "queued" are states, not failures.
  const bad = x.result && !/^(ok|success|allowed|done|unknown|queued)$/i.test(x.result);
  const detail = typeof x.detail === "object" ? JSON.stringify(x.detail) : (x.detail || "");
  const tg = (x.targets || []).join(", ");
  return `<article class="activity-row"><time datetime="${t ? t.toISOString() : ""}">${esc(t ? t.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit" }) : "Time unavailable")}</time>
    <div><h3>${esc(x.action || "Activity")}</h3><p>${esc([tg, detail].filter(Boolean).join(" · "))}</p>${x.actor ? `<p class="mono">${esc(x.actor)}${x.correlation_id ? " · " + esc(x.correlation_id) : ""}</p>` : ""}</div>
    <span class="badge ${bad ? "bad" : /^unknown$/i.test(x.result || "") ? "warn" : x.result ? "ok" : ""}">${esc(x.result || x.status || "")}</span></article>`;
}
function matches(x) {
  if (cat === "fail") return (x.result && !/^(ok|success|allowed|done|unknown|queued)$/i.test(x.result)) || /fail|error/i.test(JSON.stringify(x));
  return true;
}
function paint(empty) {
  const list = tab === "audit" ? rows.filter(matches) : rows;
  $("log-count").textContent = list.length + (tab === "audit" ? " entries" : tab === "jobs" ? " jobs" : " alerts") + (cursor ? " loaded (more available)" : "");
  $("activity-list").innerHTML = list.length ? list.map(tab === "audit" ? auditRow : tab === "jobs" ? jobRow : alertRow).join("") : `<div class="empty-state"><h2>${esc(empty || "Nothing here yet")}</h2></div>`;
  $("log-more").classList.toggle("hide", !cursor);
}
function jobRow(j) {
  const c = j.coverage || {}, t = j.created_utc_us ? new Date(j.created_utc_us / 1000) : null;
  return `<article class="activity-row"><time>${esc(t ? t.toLocaleString() : "")}</time><div><h3>${esc(j.id)}</h3><p>${V.n(c.decoded)} decoded · ${V.n(c.inferred)} inferred · ${V.n(c.skipped)} skipped · ${V.n(c.failed)} failed · ${(j.media_ids || []).length} media</p>${j.error ? `<p class="msg bad">${esc(j.error.message)}</p>` : ""}</div><span class="badge ${j.state === "failed" ? "bad" : j.state === "completed" ? "ok" : "pop"}">${esc(j.state)}</span></article>`;
}
function alertRow(a) {
  if (a.rule_id == null) { // legacy event
    const t = a.ts ? new Date(a.ts * 1000) : null;
    return `<article class="activity-row"><time>${esc(t ? t.toLocaleString() : "")}</time><div><h3>${esc(a.rule || "Alert")}</h3><p>${esc([a.camera, a.note].filter(Boolean).join(" · "))}</p></div><span class="badge">${esc(a.status || "")}</span></article>`;
  }
  const t = a.updated_utc_us ? new Date(a.updated_utc_us / 1000) : null;
  return `<article class="activity-row"><time>${esc(t ? t.toLocaleString() : "")}</time><div><h3>${esc(a.rule_id)} <span class="quiet">on ${esc(a.source_id)}</span></h3><p>${V.usTime(a.start_us)} – ${a.end_us != null ? V.usTime(a.end_us) : "ongoing"} · ${esc(a.state)}${a.actor ? " · decided by " + esc(a.actor) : ""}</p><a class="text-link" href="/review?source=${encodeURIComponent(a.source_id)}&media=${encodeURIComponent(a.media_id || "")}&start_us=${a.start_us}&end_us=${a.end_us ?? a.start_us}">Open in review</a></div><span class="badge ${a.decision === "confirmed" ? "ok" : a.decision === "rejected" ? "warn" : ""}">${esc(a.decision)}</span></article>`;
}

async function load(more) {
  const my = ++gen;
  $("logs-refresh").disabled = true; $("log-more").disabled = true; V.clear($("log-error"));
  if (!more) { rows = []; cursor = null; $("log-count").textContent = "Loading…"; }
  try {
    if (tab === "audit") {
      const q = ($("log-filter").value.trim() + " " + (cat && cat !== "fail" ? cat : "")).trim();
      const r = await V.pick(() => V.call("GET", "/api/v1/audit?q=" + encodeURIComponent(q) + (more && cursor ? "&cursor=" + encodeURIComponent(cursor) : "")), more ? null : () => V.call("GET", "/api/logs"));
      if (my !== gen) return;
      if (r.v) { const j = r.data; rows = rows.concat(V.list(j, "records").length ? V.list(j, "records") : V.list(j, "audit")); cursor = (j && j.next) || null; note = "Server-side search and pagination."; }
      else {
        const j = r.data || {}, f = q.toLowerCase();
        rows = (j.logs || []).filter((x) => !f || f.split(/\s+/).every((w) => JSON.stringify(x).toLowerCase().includes(w)));
        note = "This server version has no paged audit API: the latest " + (j.logs || []).length + " entries are filtered in your browser." + (j.index ? ` ${Number(j.index.records || 0).toLocaleString()} indexed sightings · ${j.index.storage || "local"} storage. Counts represent stored sightings, not unique people.` : "");
      }
      paint(($("log-filter").value || cat) ? "No matching activity" : "No activity recorded yet");
    } else if (tab === "jobs") {
      const r = await V.pick(() => V.call("GET", "/api/v1/jobs"));
      if (my !== gen) return;
      if (r.unavailable) { rows = []; $("log-count").textContent = ""; $("activity-list").innerHTML = '<div class="unavailable">Index jobs are not available on this server version.</div>'; note = ""; }
      else { const f = $("log-filter").value.trim().toLowerCase(); rows = V.list(r.data, "jobs").filter((j) => !f || JSON.stringify(j).toLowerCase().includes(f)); note = ""; paint("No index jobs"); }
    } else {
      const r = await V.pick(() => V.call("GET", "/api/v1/alerts" + (more && cursor ? "?cursor=" + encodeURIComponent(cursor) : "")), more ? null : () => V.call("GET", "/api/logs"));
      if (my !== gen) return;
      const f = $("log-filter").value.trim().toLowerCase();
      if (r.v) { rows = rows.concat(V.list(r.data, "alerts")); cursor = (r.data && r.data.next) || null; note = ""; }
      else { rows = (r.data.events || []).slice().reverse(); note = "Legacy alert events (no decision history on this server version)."; }
      if (f) rows = rows.filter((a) => JSON.stringify(a).toLowerCase().includes(f));
      paint(f ? "No matching alerts" : "No alerts to review");
    }
    setNote();
  } catch (e) { if (my === gen) { V.show($("log-error"), e, () => load(more)); $("log-count").textContent = ""; } }
  finally { if (my === gen) { $("logs-refresh").disabled = false; $("log-more").disabled = false; } }
}
$("log-search").onsubmit = (e) => { e.preventDefault(); load(false); };
$("log-filter").addEventListener("input", (() => { let t; return () => { clearTimeout(t); t = setTimeout(() => load(false), 350); }; })());
$("log-tabs").onclick = (e) => { const b = e.target.closest("[data-tab]"); if (!b) return; tab = b.dataset.tab; document.querySelectorAll("[data-tab]").forEach((x) => x.setAttribute("aria-pressed", String(x === b))); $("audit-tools").classList.toggle("hide", tab !== "audit"); load(false); };
$("log-cats").onclick = (e) => { const b = e.target.closest("[data-cat]"); if (!b) return; cat = b.dataset.cat; document.querySelectorAll("#log-cats button").forEach((x) => x.setAttribute("aria-pressed", String(x === b))); load(false); };
$("logs-refresh").onclick = () => load(false);
$("log-more").onclick = () => load(true);
V.auth(); load(false); Watch.start(null, 3000);
