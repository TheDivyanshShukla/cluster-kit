// Cluster page: PCs, realtime capacity, benchmark, phone camera. Talks to /api/v1/cluster, /cluster/realtime, /benchmarks, /sources.
(() => {
  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const n1 = (x) => (x == null || !Number.isFinite(Number(x)) ? "-" : String(Math.round(Number(x) * 10) / 10));
  const when = (t) => (t ? new Date(t * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "");
  const dur = (s) => (s == null ? "-" : s >= 120 ? Math.floor(s / 60) + " min " + String(Math.round(s % 60)).padStart(2, "0") + " s" : n1(s) + " s");
  const short = (id) => String(id || "").split("|")[0] + (String(id || "").split("|").length > 2 ? "#" + String(id).split("|")[2] : "");

  async function api(url, opt) {
    try {
      const r = await fetch(url, Object.assign({ credentials: "same-origin" }, opt));
      const body = await r.json().catch(() => ({}));
      return { ok: r.ok, status: r.status, body };
    } catch (e) { return { ok: false, status: 0, body: { error: { message: String(e) } } }; }
  }
  const post = (url, b) => api(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(b) });
  const errText = (r) => (r.body && r.body.error && r.body.error.message) || (r.status === 401 ? "Sign in first." : "Request failed (" + r.status + ").");

  // ---------------------------------------------------------------- PCs
  let nodes = [];
  function sameMachine(list) {
    const ips = list.filter((n) => n.state !== "lost").map((n) => n.ip || n.host);
    return ips.length > new Set(ips).size;
  }
  async function loadNodes() {
    const r = await api("/api/v1/cluster");
    if (!r.ok) return;
    nodes = r.body.nodes || [];
    document.querySelector("#nodes tbody").innerHTML = nodes.map((n) => {
      const cls = n.state === "lost" ? "bad" : n.state === "slow" ? "warn" : "ok";
      const work = (n.tasks || []).map((t) => esc(t.camera || t.source || t.task_id)).join(", ") || '<span class="quiet">nothing</span>';
      return `<tr class="${n.state === "lost" ? "lost" : ""}"><td>${esc(n.label || n.host)}${n.local ? ' <span class="tag">head</span>' : ""}</td><td>${esc(n.os)} ${esc(n.arch)}</td><td class="n">${n.cores}</td><td><span class="state"><i class="dot ${cls}"></i>${esc(n.state)}</span></td><td>${work}</td><td class="n">${n1(n.measured_fps)}</td><td class="n">${n1(n.frame_ms)}</td><td class="n">${Math.round(n.cpu || 0)}%</td><td>${esc(when(n.joined))}</td></tr>`;
    }).join("") || `<tr><td colspan="9" class="quiet">No PC has reported yet.</td></tr>`;
    const w = $("same-host");
    const alive = nodes.filter((n) => n.state !== "lost");
    if (sameMachine(nodes)) {
      w.hidden = false;
      w.textContent = "Some workers run on the same machine. They share its CPU: do not expect a cluster speed-up from them. A gain here only means one worker was not using the whole machine.";
    } else w.hidden = true;
    $("bench-workers").textContent = alive.length + " PC" + (alive.length === 1 ? "" : "s") + " online";
    renderDevices();
  }

  // ---------------------------------------------------------------- realtime
  function meter(frac, cls) {
    const f = Math.max(0, Math.min(1, frac || 0));
    return `<div class="meter ${cls || ""}" role="img" aria-label="${Math.round(f * 100)}%"><i style="width:${f * 100}%"></i></div>`;
  }
  function paintRealtime(rt) {
    if (!rt) return;
    $("rt-age").textContent = new Date().toLocaleTimeString();
    const t = rt.total || {};
    const cams = rt.cameras || [];
    if (!cams.length) {
      $("rt-total").innerHTML = '<div class="empty-state"><h2>No live camera is running</h2><p>Add a phone camera below, or a camera on the Sources page. Each live camera is then shown here with the PC that analyses it.</p></div>';
      $("rt-map").innerHTML = "";
      document.querySelector("#rt-cams tbody").innerHTML = "";
      $("rt-moves").hidden = true;
      return;
    }
    const known = t.capacity_fps > 0;
    const load = known ? t.demand_fps / t.capacity_fps : 0;
    $("rt-total").innerHTML = `<div class="row" style="justify-content:space-between"><b>${n1(t.demand_fps)} fps needed</b><span class="mono quiet">${known ? n1(t.capacity_fps) + " fps capacity, headroom " + n1(t.headroom_fps) + " fps" : "capacity not measured yet"}</span></div>${meter(load, load > 1 ? "bad" : load > 0.75 ? "warn" : "ok")}<p class="quiet">${t.workers} PC${t.workers === 1 ? "" : "s"} for ${t.cameras} camera${t.cameras === 1 ? "" : "s"}${known && load > 1 ? ". Demand is above capacity: cameras will fall behind. Join another PC." : ""}</p>`;
    $("rt-map").innerHTML = (rt.workers || []).map((w) => {
      const mine = cams.filter((c) => c.worker === w.id);
      const l = w.load;
      return `<div class="map-row"><span class="host"><b>${esc(short(w.id))}</b> <span class="quiet mono">${w.capacity_fps != null ? n1(w.demand_fps) + "/" + n1(w.capacity_fps) + " fps" : ""} ${Math.round(w.cpu || 0)}% cpu</span></span><span style="flex:0 0 110px">${meter(l, l > 1 ? "bad" : l > 0.75 ? "warn" : "ok")}</span>${mine.map((c) => `<span class="chip ${c.status === "keeping up" ? "" : esc(c.status)}">${esc(c.name)} <span class="mono">${n1(c.fps)} fps</span></span>`).join("") || '<span class="quiet">idle</span>'}</div>`;
    }).join("");
    document.querySelector("#rt-cams tbody").innerHTML = cams.map((c) => {
      const cls = c.status === "keeping up" ? "keeping" : c.status;
      return `<tr><td>${esc(c.name)}</td><td>${esc(short(c.worker))}</td><td class="n">${n1(c.fps)}</td><td class="n">${n1(c.required_fps)}</td><td class="n">${n1(c.lag_s)} s</td><td class="status-${cls}">${esc(c.status)}${c.dropped_per_s > 0.05 ? ' <span class="quiet">(' + n1(c.dropped_per_s) + "/s dropped)</span>" : ""}${c.error ? ' <span class="quiet">' + esc(c.error) + "</span>" : ""}</td></tr>`;
    }).join("");
    const mv = rt.moves || [];
    $("rt-moves").hidden = !mv.length;
    $("rt-moves").innerHTML = mv.length ? "<b>Recent moves</b><br>" + mv.map((m) => `${esc(new Date(m.ts * 1000).toLocaleTimeString())}: ${esc(m.camera)} left ${esc(short(m.from))}. ${esc(m.why)}`).join("<br>") : "";
  }
  async function loadRealtime() { const r = await api("/api/v1/cluster/realtime"); if (r.ok) paintRealtime(r.body); }
  try {
    const es = new EventSource("/api/v1/events");
    es.addEventListener("realtime", (e) => { try { paintRealtime(JSON.parse(e.data)); } catch (_) {} });
    es.onerror = () => {};
  } catch (_) {}

  // ---------------------------------------------------------------- benchmark
  let media = [], runs = [], running = null, pollTimer = null;
  async function loadMedia() {
    const r = await api("/api/v1/benchmarks/media");
    media = r.ok ? r.body.media || [] : [];
    const box = $("b-media");
    box.innerHTML = media.map((m, i) => `<label><input type="checkbox" data-i="${i}"> ${esc(m.name || m.id)} <small>${(m.size / 1048576).toFixed(0)} MB</small></label>`).join("") || '<span class="quiet">No recordings found in vision/footage.</span>';
  }
  function bars(rows, fmt, max, ideal) {
    return `<div class="cl-bars">${rows.map((r) => `<span>${esc(r.label)}</span><div class="meter ${r.cls || ""}"><i style="width:${Math.min(100, (r.v / max) * 100)}%"></i>${ideal && r.ideal ? `<span class="mark" title="ideal ${n1(r.ideal)}" style="left:${Math.min(100, (r.ideal / max) * 100)}%"></span>` : ""}</div><span>${fmt(r)}</span>`).join("")}</div>`;
  }
  function paintLive() {
    const el = $("b-live");
    if (!running) { el.innerHTML = '<p class="quiet">Pick one or more recordings and press Run. Progress appears here, per PC.</p>'; return; }
    const r = running;
    el.innerHTML = `<h3>${esc(r.name)}</h3>
      <div class="row" style="justify-content:space-between"><b>${Math.round((r.progress || 0) * 100)}%</b><span class="mono quiet">${dur(r.elapsed_s)} elapsed${r.eta_s != null ? ", about " + dur(r.eta_s) + " left" : ""}</span></div>
      ${meter(r.progress, "ok")}
      <p class="quiet mono">${r.tasks_done}/${r.tasks} parts done, ${n1(r.media_s)} s of video</p>
      ${bars((r.workers || []).map((w) => ({ label: short(w.id), v: w.share || 0, cls: "", share: w.share })), (x) => Math.round((x.share || 0) * 100) + "%", 1) || ""}
      ${(r.workers || []).length ? "" : '<p class="quiet">Waiting for a PC to take the first part.</p>'}`;
  }
  function paintResults() {
    const done = runs.filter((r) => r.state !== "running");
    document.querySelector("#b-table tbody").innerHTML = done.map((r) => {
      const ok = r.state === "done";
      const w = (r.workers || []).map((x) => short(x.id)).join(", ");
      return `<tr><td>${esc(r.name)}<br><span class="quiet">${esc(when(r.started))}${r.state !== "done" ? " - " + esc(r.state) : ""}</span></td><td class="n" title="${esc(w)}">${r.worker_count ?? ""}${r.same_machine ? ' <span class="tag warn" title="workers shared one machine">1 machine</span>' : ""}</td><td class="n">${ok ? dur(r.wall_s) : "-"}</td><td class="n">${ok ? n1(r.speed_x) + "x realtime" : "-"}</td><td class="n">${r.frames ? r.frames.inferred.toLocaleString() : "-"}</td><td class="n">${r.speedup != null ? n1(r.speedup) + "x" : "-"}</td><td class="n">${r.efficiency != null ? Math.round(r.efficiency * 100) + "%" : "-"}</td><td><button class="sm" data-del="${esc(r.id)}" type="button">Delete</button></td></tr>`;
    }).join("") || `<tr><td colspan="8" class="quiet">No benchmark has run yet.</td></tr>`;
    // Chart: runs that did the same work as the newest finished run, fewest PCs first.
    const fin = done.filter((r) => r.state === "done");
    const chart = $("b-chart");
    if (!fin.length) { chart.innerHTML = ""; return; }
    const group = fin.filter((r) => r.signature === fin[0].signature).sort((a, b) => (a.worker_count - b.worker_count) || (a.started - b.started));
    const maxT = Math.max(...group.map((r) => r.wall_s));
    const hasBase = group.some((r) => r.speedup != null);
    let h = `<h3 style="margin-top:1rem">Same recordings and settings: ${group.length} run${group.length === 1 ? "" : "s"}</h3>`;
    h += "<p class='quiet' style='margin:.2rem 0'>Time (shorter is better)</p>" + bars(group.map((r) => ({ label: r.worker_count + " PC" + (r.worker_count === 1 ? "" : "s") + " - " + r.name, v: r.wall_s, cls: r.same_machine ? "warn" : "" })), (r) => dur(r.v), maxT);
    if (hasBase) {
      const maxS = Math.max(...group.map((r) => Math.max(r.speedup || 0, r.worker_count)));
      h += "<p class='quiet' style='margin:.8rem 0 .2rem'>Speed-up against the 1-PC run (the mark is the ideal: as many times faster as PCs)</p>" + bars(group.filter((r) => r.speedup != null).map((r) => ({ label: r.worker_count + " PC" + (r.worker_count === 1 ? "" : "s"), v: r.speedup, ideal: r.worker_count, cls: r.same_machine ? "warn" : "ok", eff: r.efficiency })), (r) => n1(r.v) + "x, " + Math.round(r.eff * 100) + "% efficient", maxS, true);
    } else {
      h += `<p class="cl-note">Only the runs above exist so far${group.every((r) => r.worker_count > 1) ? ", and none used a single PC" : ""}. Run again with a different number of PCs to see a speed-up. Nothing is estimated.</p>`;
    }
    chart.innerHTML = h;
    const w = $("b-warn");
    const shared = group.some((r) => r.same_machine) || sameMachine(nodes);
    w.hidden = !shared;
    w.textContent = shared ? "Workers on one machine share the CPU: no cluster speed-up is expected. A gain in such a run only means one worker was not using the whole machine; it measures that machine, not a cluster." : "";
  }
  async function loadRuns() {
    const r = await api("/api/v1/benchmarks");
    if (!r.ok) return;
    runs = r.body.runs || [];
    running = r.body.running || null;
    paintLive(); paintResults();
    $("b-run").disabled = !!running;
    if (running && !pollTimer) pollTimer = setInterval(loadRuns, 1000);
    if (!running && pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  }
  $("b-run").addEventListener("click", async () => {
    const picked = [...document.querySelectorAll("#b-media input:checked")].map((c) => media[Number(c.dataset.i)].id);
    const msg = $("b-msg");
    msg.className = "msg";
    if (!picked.length) { msg.className = "msg bad"; msg.textContent = "Pick at least one recording."; return; }
    const body = { name: $("b-name").value.trim() || (nodes.filter((n) => n.state !== "lost").length + " PC run"), media: picked, repeat: Number($("b-rep").value) || 1 };
    if ($("b-seg").value !== "") body.segment_secs = Number($("b-seg").value);
    const r = await post("/api/v1/benchmarks", body);
    if (!r.ok) { msg.className = "msg bad"; msg.textContent = errText(r); return; }
    msg.textContent = "";
    await loadRuns();
  });
  document.addEventListener("click", async (e) => {
    const b = e.target.closest("[data-del]");
    if (!b) return;
    if (!confirm("Delete this run and the data it indexed?")) return;
    await api("/api/v1/benchmarks/" + encodeURIComponent(b.dataset.del), { method: "DELETE" });
    loadRuns();
  });

  // ---------------------------------------------------------------- phone and local cameras
  let tested = null;
  const slug = (s) => (String(s).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "phone").slice(0, 40);
  $("ph-test").addEventListener("click", async () => {
    const url = $("ph-url").value.trim();
    const msg = $("ph-msg");
    tested = null; $("ph-add").disabled = true;
    msg.className = "msg"; msg.textContent = "Testing (up to 5 seconds)...";
    $("ph-test").disabled = true;
    const r = await post("/api/v1/sources/probe", { url });
    $("ph-test").disabled = false;
    if (!r.ok) { msg.className = "msg bad"; msg.textContent = errText(r); return; }
    const v = r.body;
    if (!v.ok) { msg.className = "msg bad"; msg.textContent = v.error; return; }
    tested = url;
    msg.className = "msg ok";
    msg.textContent = `Works: ${v.width}x${v.height}${v.fps ? ", " + v.fps + " fps" : ""}, ${v.codec || "video"}, answered in ${v.latency_ms} ms.`;
    $("ph-add").disabled = false;
  });
  $("ph-url").addEventListener("input", () => { tested = null; $("ph-add").disabled = true; });
  async function addSource(body, msg) {
    const r = await post("/api/v1/sources", body);
    msg.className = r.ok ? "msg ok" : "msg bad";
    msg.textContent = r.ok ? "Added. It appears under Realtime once a PC starts it." : errText(r);
    if (r.ok) { setTimeout(loadRealtime, 2500); }
  }
  $("ph-add").addEventListener("click", () => {
    if (!tested) return;
    const name = $("ph-name").value.trim() || "Phone";
    addSource({ id: slug(name), name, kind: "live", location: tested }, $("ph-msg"));
  });
  function renderDevices() {
    const el = $("devices");
    const list = [];
    for (const n of nodes) {
      if (n.state === "lost") continue;
      const d = n.devices || [];
      for (let i = 0; i < d.length; i++) if (/^\d+$/.test(d[i])) list.push({ node: n, index: d[i], name: d[i + 1] && !/^\d+$/.test(d[i + 1]) ? d[i + 1] : "Camera " + d[i] });
    }
    const sig = JSON.stringify(list.map((c) => [c.node.id, c.index, c.name]));
    if (el.dataset.sig === sig) return;
    el.dataset.sig = sig;
    if (!list.length) { el.innerHTML = '<p class="quiet">No camera is attached to any PC that is online. A phone stream (left) works without one.</p>'; return; }
    el.innerHTML = list.map((c, i) => {
      const iphone = /iphone|continuity/i.test(c.name);
      const label = iphone ? "Use iPhone (Continuity Camera)" : c.node.os === "macos" ? "Use this Mac's camera" : "Use this camera";
      return `<div class="map-row"><span class="host"><b>${esc(c.name)}</b><br><span class="quiet">on ${esc(c.node.label || c.node.host)}</span></span><button class="sm" data-dev="${i}" type="button">${label}</button></div>`;
    }).join("") + '<div class="msg" id="dev-msg" role="status"></div>';
    el.querySelectorAll("[data-dev]").forEach((b) => b.addEventListener("click", () => {
      const c = list[Number(b.dataset.dev)];
      addSource({ id: slug(c.name + "-" + (c.node.label || c.node.host)), name: c.name, kind: "webcam", location: c.index }, $("dev-msg"));
    }));
  }

  loadNodes(); loadRealtime(); loadMedia(); loadRuns();
  setInterval(loadNodes, 3000);
  setInterval(loadRealtime, 2500);
  setInterval(() => { if (!running) loadRuns(); }, 8000);
})();
