const Watch = (() => {
  const $ = (id) => document.getElementById(id);

  function esc(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }
  function css(s) { return String(s).replace(/\\/g, "\\\\").replace(/"/g, '\\"'); }
  function num(v) {
    const x = Number(v);
    return Number.isFinite(x) ? String(Math.round(x * 100) / 100) : "";
  }
  function clipTime(at) {
    const s = Math.max(0, Math.floor(Number(at) || 0));
    return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0");
  }
  function when(ts) {
    if (!ts) return "";
    const d = new Date(ts * 1000);
    return d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  }
  function fromLocal(v) {
    if (!v) return 0;
    const t = new Date(v).getTime();
    return Number.isFinite(t) ? t / 1000 : 0;
  }
  function toLocal(ts) {
    if (!ts) return "";
    const d = new Date(ts * 1000);
    const p = (n) => String(n).padStart(2, "0");
    return d.getFullYear() + "-" + p(d.getMonth() + 1) + "-" + p(d.getDate()) + "T" + p(d.getHours()) + ":" + p(d.getMinutes());
  }
  function lineFor(cls, op, n) {
    const q = JSON.stringify(String(cls || "").trim() || "object");
    if (op === "seen") return "seen(" + q + ")";
    const sym = { gt: ">", lt: "<", ge: ">=", le: "<=", eq: "==" }[op] || ">";
    return "count(" + q + ") " + sym + " " + (Number(n) || 0);
  }
  async function post(url, body) {
    const r = await fetch(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error((j.error && j.error.message) || j.error || "Could not save");
    return j;
  }
  function boxes(svg, list) {
    const items = Array.isArray(list) ? list : [];
    svg.setAttribute("viewBox", "0 0 100 100");
    svg.innerHTML = items.map((b) => {
      if ((b.w || 0) < 0.01 || (b.h || 0) < 0.01) return "";
      const x = (b.x || 0) * 100, y = (b.y || 0) * 100, w = (b.w || 0) * 100, h = (b.h || 0) * 100;
      const label = [b.color, b.class].filter(Boolean).join(" ");
      return `<rect x="${x}" y="${y}" width="${w}" height="${h}"></rect><text x="${x + 0.6}" y="${Math.min(97, y + 3.2)}" font-size="2.6" stroke="#0A0A0A" stroke-width="0.45" paint-order="stroke">${esc(label)}</text>`;
    }).join("");
  }
  function meters(s) {
    if (!$("f-ops") || !s.figures) return;
    Watch.count($("f-ops"), s.figures.ops); Watch.count($("f-lack"), s.figures.lacking);
    Watch.count($("f-pcs"), s.figures.pcs); Watch.count($("f-warn"), s.figures.warnings);
  }
  function start(paint, ms) {
    async function tick() {
      try {
        const r = await fetch("/api/state");
        if (!r.ok) throw new Error("Engine returned " + r.status);
        const s = await r.json();
        const connection = $("connection");
        if (connection) { connection.textContent = ""; connection.classList.add("hide"); }
        Watch.state = s;
        meters(s);
        if (paint) paint(s);
      } catch (_) {
        const connection = $("connection");
        if (connection) { connection.textContent = "Connection to the engine was lost. Showing the last received state; retrying automatically."; connection.classList.remove("hide"); }
      }
    }
    tick();
    setInterval(tick, ms || 500);
  }
  function fold(hits) {
    const out = [];
    const sorted = (hits || []).slice().sort((a, b) => (a.at || 0) - (b.at || 0));
    for (const h of sorted) {
      const prev = out.find((o) => o.camera_id === h.camera_id && o.class === h.class && o.color === h.color && Math.abs((o.at || 0) - (h.at || 0)) < 2);
      if (prev) prev.n += 1;
      else out.push(Object.assign({}, h, { n: 1 }));
    }
    return out;
  }
  return { $, state: null, esc, css, num, clipTime, when, fromLocal, toLocal, lineFor, post, boxes, fold, start };
})();

/* ---- motion helpers (shared): reduced-motion check, tweened counters, list fill with entrance ---- */
Watch.reduced = () => !!(window.matchMedia && matchMedia("(prefers-reduced-motion: reduce)").matches);
// Tween the number shown in `el` to `v` (ease-out, ~350 ms). Same target twice is a no-op, so polling never restarts it.
Watch.count = (el, v) => {
  const to = Number(v);
  if (!el) return;
  if (!Number.isFinite(to)) { el.textContent = v; el._to = undefined; return; }
  if (el._to === to) return;
  const from = Number.isFinite(el._cur) ? el._cur : to, dec = Math.min(2, (String(v).split(".")[1] || "").length);
  el._to = to; cancelAnimationFrame(el._raf);
  if (Watch.reduced() || from === to) { el._cur = to; el.textContent = to.toFixed(dec); return; }
  const t0 = performance.now();
  (function step(now) {
    const k = Math.min(1, (now - t0) / 350), e = 1 - Math.pow(1 - k, 3);
    el._cur = from + (to - from) * e; el.textContent = el._cur.toFixed(dec);
    if (k < 1) el._raf = requestAnimationFrame(step);
  })(t0);
};
// Set innerHTML only when it changed; the first non-empty fill plays the staggered entrance.
Watch.fill = (el, html) => {
  if (!el || el._h === html) return false;
  const first = !el.firstElementChild;
  el._h = html; el.innerHTML = html;
  if (first && html && !Watch.reduced()) { el.classList.remove("enter"); void el.offsetWidth; el.classList.add("enter"); }
  return true;
};

/* ---- shared detection annotation renderer (Live wall + Review overlay) ----
   const an = Watch.Annotate.create(); an.update(dets); an.paint(ctx, {x,y,w,h}, performance.now(), opts) -> still animating?
   dets: [{class, confidence, bbox:[x0,y0,x1,y1] normalized, track_id}]. The rect {x,y,w,h} is the image content rect,
   so boxes stay aligned to the picture. Boxes tween (180 ms) between frames while a track persists, fade/scale in when
   new, fade out (300 ms) when lost. Hue is stable per track id (golden-angle hash), class-tinted without a track. */
Watch.Annotate = (() => {
  const TWEEN = 180, FADE_IN = 220, FADE_OUT = 300, TRAIL = 14;
  const ease = (k) => 1 - Math.pow(1 - k, 3);
  const hash = (s) => { let h = 2166136261; for (let i = 0; i < s.length; i++) { h = Math.imul(h ^ s.charCodeAt(i), 16777619); } return h >>> 0; };
  const hueOf = (cls, tid) => tid != null ? ((hash("t" + tid) * 0.6180339887) % 1) * 360 : (cls === "person" ? 152 : (hash(cls) % 12) * 30 + 15);
  const iou = (a, b) => {
    const w = Math.min(a[2], b[2]) - Math.max(a[0], b[0]), h = Math.min(a[3], b[3]) - Math.max(a[1], b[1]);
    if (w <= 0 || h <= 0) return 0;
    const i = w * h; return i / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - i);
  };
  // opts.direct: caller supplies already-interpolated dets (with optional d.alpha); no tween/fade of our own, longer trail.
  let FONT = "";
  function create(opts) {
    const direct = !!(opts && opts.direct), TR = direct ? 30 : TRAIL;
    const states = new Map(); let seq = 0, src = null, count = 0, countT = -1e9;
    const an = { busy: false };
    const pos = (s, now) => {
      const k = s.dur ? Math.min(1, (now - s.t0) / s.dur) : 1, e = ease(k);
      return s.from.map((v, i) => v + (s.to[i] - v) * e);
    };
    an.clear = () => { states.clear(); src = null; an.busy = false; };
    an.update = (dets, now) => {
      if (dets === src) return; src = dets;
      now = now ?? performance.now();
      const red = Watch.reduced() || direct, used = new Set();
      for (const d of dets || []) {
        if (!d.bbox || d.bbox.length < 4) continue;
        let s = d.track_id != null ? states.get("t" + d.track_id) : null;
        if (!s && d.track_id == null) {
          let best = 0.15;
          for (const c of states.values()) if (!used.has(c) && !c.tracked && !c.lostAt && c.cls === d.class) { const v = iou(c.to, d.bbox); if (v > best) { best = v; s = c; } }
        }
        const bb = d.bbox.slice(0, 4);
        if (s) {
          s.from = pos(s, now); s.to = bb; s.t0 = now; s.dur = red ? 0 : TWEEN; s.lostAt = 0;
        } else {
          s = { key: d.track_id != null ? "t" + d.track_id : "u" + (++seq), tracked: d.track_id != null, cls: d.class, from: bb, to: bb, t0: now, dur: 0, born: red ? -1e9 : now, lostAt: 0, trail: [], tid: d.track_id };
          states.set(s.key, s);
        }
        s.alpha = d.alpha == null ? 1 : d.alpha; s.conf = d.confidence; s.cls = d.class; s.hue = hueOf(d.class, d.track_id);
        s.trail.push([(bb[0] + bb[2]) / 2, bb[3]]); if (s.trail.length > TR) s.trail.shift();
        used.add(s);
      }
      for (const s of states.values()) if (!used.has(s) && !s.lostAt) { s.from = s.to = pos(s, now); s.dur = 0; s.lostAt = red ? -1e9 : now; }
      const n = [...used].filter((s) => s.cls === "person").length;
      if (n !== count) { count = n; countT = now; }
    };
    an.paint = (g, r, now, o) => {
      o = Object.assign({ boxes: true, labels: true, conf: true, trail: false, thresh: 0.5, badge: true }, o);
      let busy = false;
      g.save(); g.lineCap = "round"; g.lineJoin = "round"; g.font = "11px " + (FONT || (FONT = getComputedStyle(document.documentElement).getPropertyValue("--font-mono") || "monospace")); g.textBaseline = "middle";
      for (const [key, s] of states) {
        const inK = Math.min(1, (now - s.born) / FADE_IN), outK = s.lostAt ? 1 - (now - s.lostAt) / FADE_OUT : 1;
        if (outK <= 0) { states.delete(key); continue; }
        const [x0, y0, x1, y1] = pos(s, now);
        if (inK < 1 || s.lostAt || (s.dur && now - s.t0 < s.dur)) busy = true;
        const a = ease(inK) * (s.lostAt ? ease(outK) : 1) * (s.alpha == null ? 1 : s.alpha), sc = 0.94 + 0.06 * ease(inK);
        let w = (x1 - x0) * r.w, h = (y1 - y0) * r.h; const cx = r.x + (x0 + x1) / 2 * r.w, cy = r.y + (y0 + y1) / 2 * r.h;
        if (w < 2 || h < 2) continue;
        w *= sc; h *= sc; const x = cx - w / 2, y = cy - h / 2, col = `hsl(${s.hue} 90% 62%)`, low = s.conf < o.thresh;
        if (o.trail && s.trail.length > 1) {
          g.lineWidth = 1.5; g.strokeStyle = col;
          for (let i = 1; i < s.trail.length; i++) {
            g.globalAlpha = a * 0.55 * (i / s.trail.length); g.beginPath();
            g.moveTo(r.x + s.trail[i - 1][0] * r.w, r.y + s.trail[i - 1][1] * r.h); g.lineTo(r.x + s.trail[i][0] * r.w, r.y + s.trail[i][1] * r.h); g.stroke();
          }
        }
        if (o.boxes) {
          g.globalAlpha = a * (low ? 0.75 : 1); g.strokeStyle = col; g.fillStyle = `hsl(${s.hue} 90% 62% / .07)`; g.fillRect(x, y, w, h);
          if (low) { g.lineWidth = 1.25; g.setLineDash([5, 4]); g.strokeRect(x, y, w, h); g.setLineDash([]); }
          else {
            const L = Math.max(7, Math.min(20, Math.min(w, h) * 0.22));
            g.shadowColor = `hsl(${s.hue} 95% 60% / .75)`; g.shadowBlur = 6; g.lineWidth = 2; g.beginPath();
            g.moveTo(x, y + L); g.lineTo(x, y); g.lineTo(x + L, y); g.moveTo(x + w - L, y); g.lineTo(x + w, y); g.lineTo(x + w, y + L);
            g.moveTo(x + w, y + h - L); g.lineTo(x + w, y + h); g.lineTo(x + w - L, y + h); g.moveTo(x + L, y + h); g.lineTo(x, y + h); g.lineTo(x, y + h - L);
            g.stroke(); g.shadowBlur = 0;
          }
        }
        if (o.labels) {
          const pct = Math.round((s.conf || 0) * 100), text = s.cls + (o.conf ? " " + pct + "%" : ""), tw = g.measureText(text).width + 12, ch = 16;
          const cyy = y >= ch + 3 ? y - ch - 2 : y + 2, cxx = Math.min(x, Math.max(r.x, r.x + r.w - tw));
          g.globalAlpha = a * (low ? 0.8 : 1); g.fillStyle = "rgba(8,9,10,.8)"; g.fillRect(cxx, cyy, tw, ch);
          g.fillStyle = col; g.fillRect(cxx, cyy, 2, ch);
          g.fillStyle = "#E6E8EA"; g.fillText(text, cxx + 7, cyy + ch / 2 - 1);
          if (o.conf) { g.fillStyle = "rgba(255,255,255,.14)"; g.fillRect(cxx + 2, cyy + ch - 2, tw - 2, 2); g.fillStyle = col; g.fillRect(cxx + 2, cyy + ch - 2, (tw - 2) * Math.max(0, Math.min(1, s.conf || 0)), 2); }
        }
      }
      g.globalAlpha = 1;
      if (o.badge && count) {
        const bump = Math.max(0, 1 - (now - countT) / 220); if (bump > 0) busy = true;
        const text = count + (count === 1 ? " person" : " people"), tw = g.measureText(text).width + 22, bx = o.bx ?? r.x + 8, by = o.by ?? r.y + 8, k = 1 + 0.12 * bump;
        g.translate(bx, by + 8); g.scale(k, k);
        g.fillStyle = "rgba(8,9,10,.78)"; g.fillRect(0, -8, tw, 16);
        g.fillStyle = "hsl(152 90% 62%)"; g.beginPath(); g.arc(8, 0, 3, 0, 6.2832); g.fill();
        g.fillStyle = "#E6E8EA"; g.fillText(text, 16, 0);
      }
      g.restore(); an.busy = busy; return busy;
    };
    return an;
  }
  return { create };
})();

/* ---- code editor (textarea over highlighted <pre>), shared by Search and Rules & models ---- */
Watch.Editor = (() => {
  const esc = Watch.esc;
const SQL_KW = new Set("select from where and or not in like group by order limit offset count distinct as between is null true false having join on asc desc sum avg min max".split(" "));
const SQL_ID = new Set("class confidence camera camera_id color ts at track_id near_person model id source_id media_id capture_time_utc_us media_time_us".split(" "));
const LUA_KW = new Set("and break do else elseif end false for function goto if in local nil not or repeat return then true until while".split(" "));
const LUA_FN = new Set("ipairs pairs string table math print type tostring tonumber select next unpack os".split(" "));
const RX = {
  sql: /(--[^\n]*)|('(?:[^']|'')*'?)|(\b\d+(?:\.\d+)?\b)|(\b[A-Za-z_][A-Za-z0-9_]*\b)|([<>=!*+\-\/%(),;]+)/g,
  lua: /(--\[\[[\s\S]*?(?:\]\]|$)|--[^\n]*)|("(?:[^"\\\n]|\\.)*"?|'(?:[^'\\\n]|\\.)*'?|\[\[[\s\S]*?(?:\]\]|$))|(\b\d+(?:\.\d+)?\b)|(\b[A-Za-z_][A-Za-z0-9_]*\b)|([<>=~!*+\-\/%^#(){}\[\],;.:]+)/g
};
function highlight(src, lang) {
  let out = "", last = 0, m; const rx = new RegExp(RX[lang].source, "g");
  while ((m = rx.exec(src))) {
    out += esc(src.slice(last, m.index)); last = rx.lastIndex;
    const t = m[0]; let c = "";
    if (m[1] != null && m[1] !== "") c = "tk-com"; else if (m[2] != null && m[2] !== "") c = "tk-str"; else if (m[3] != null && m[3] !== "") c = "tk-num";
    else if (m[4] != null && m[4] !== "") { const w = t.toLowerCase(); c = lang === "sql" ? (SQL_KW.has(w) ? "tk-kw" : SQL_ID.has(w) ? "tk-id" : "") : (LUA_KW.has(t) ? "tk-kw" : LUA_FN.has(t) ? "tk-fn" : (src[last] === "(" ? "tk-fn" : (t === "hit" ? "tk-id" : ""))); }
    else if (m[5]) c = "tk-op";
    out += c ? `<span class="${c}">${esc(t)}</span>` : esc(t);
  }
  return out + esc(src.slice(last));
}
/* ---- completion catalogs (static; values come from the server when available) ---- */
const COCO = "person,bicycle,car,motorcycle,airplane,bus,train,truck,boat,traffic light,fire hydrant,stop sign,parking meter,bench,bird,cat,dog,horse,sheep,cow,elephant,bear,zebra,giraffe,backpack,umbrella,handbag,tie,suitcase,frisbee,skis,snowboard,sports ball,kite,baseball bat,baseball glove,skateboard,surfboard,tennis racket,bottle,wine glass,cup,fork,knife,spoon,bowl,banana,apple,sandwich,orange,broccoli,carrot,hot dog,pizza,donut,cake,chair,couch,potted plant,bed,dining table,toilet,tv,laptop,mouse,remote,keyboard,cell phone,microwave,oven,toaster,sink,refrigerator,book,clock,vase,scissors,teddy bear,hair drier,toothbrush".split(",");
// Filled from GET /api/v1/models and /api/v1/sources on first focus; static fallbacks otherwise.
const DATA = { labels: COCO, sources: [], versions: [], holdingOk: false, legacy: false, loaded: false };
let loading = null;
function loadData() {
  if (loading) return loading;
  const get = (u) => fetch(u, { credentials: "same-origin" }).then((r) => (r.ok ? r.json() : null)).catch(() => null);
  loading = Promise.all([get("/api/v1/models"), get("/api/v1/sources")]).then(([m, s]) => {
    const ms = (m && Array.isArray(m.models) ? m.models : []);
    const labels = [...new Set(ms.flatMap((e) => (e.model && e.model.labels) || []))];
    if (labels.length) DATA.labels = labels.sort();
    DATA.versions = [...new Set(ms.map((e) => e.model && e.model.id).filter(Boolean))];
    if (m && m.support) DATA.holdingOk = !!m.support.holding && m.support.holding !== "unsupported";
    const src = Array.isArray(s) ? s : (s && (s.sources || s.items)) || [];
    DATA.sources = src.map((x) => ({ id: x.id, name: x.name || x.id })).filter((x) => x.id);
    DATA.loaded = true;
  });
  return loading;
}
const I = (l, k, d, doc, o) => Object.assign({ l, k, d: d || "", doc: doc || "", ins: l }, o || {});
const HOLD = "needs a relation model: currently unsupported";
const holdBad = () => (DATA.holdingOk ? "" : HOLD);

/* SQL: exactly what rust/src/watch/query/v1.rs parses (no LIKE, BETWEEN, GROUP BY, OFFSET, joins, comments). */
const SQL_DOC = {
  SELECT: ["statement", "Starts a query: * | DISTINCT column | COUNT(*) | COUNT(DISTINCT column).", "SELECT * FROM detections LIMIT 20"],
  FROM: ["clause", "Names the table: detections, tracks, relations or frames.", "SELECT * FROM tracks"],
  WHERE: ["clause", "Conditions on columns of the table. Combine with AND, OR, NOT and parentheses.", "WHERE class = 'person' AND confidence >= 0.5"],
  AND: ["operator", "Both conditions must hold. Binds tighter than OR.", "class = 'car' AND confidence > 0.6"],
  OR: ["operator", "Either condition may hold.", "class = 'car' OR class = 'truck'"],
  NOT: ["operator", "Negates the next condition; also NOT IN.", "NOT class = 'person'"],
  IN: ["operator", "Matches any value of a list of quoted strings or numbers (same type as the column).", "class IN ('car', 'bus')"],
  "NOT IN": ["operator", "Matches none of the listed values.", "class NOT IN ('person')"],
  "ORDER BY": ["clause", "Sorts by media_time_us only (not on relations).", "ORDER BY media_time_us DESC"],
  ASC: ["order", "Oldest media time first (default).", ""], DESC: ["order", "Newest media time first.", ""],
  LIMIT: ["clause", "Maximum rows, an integer.", "LIMIT 100"],
  DISTINCT: ["modifier", "SELECT DISTINCT column: unique values of one column.", "SELECT DISTINCT class FROM detections"],
  COUNT: ["aggregate", "COUNT(*) rows, or COUNT(DISTINCT column) unique values (e.g. distinct tracks).", "SELECT COUNT(DISTINCT track_id) FROM detections WHERE class = 'person'"]
};
const SQL_TABLES = {
  detections: ["one row per detected object", "Every detection the models produced (class, confidence, track, time). Counts are observations, not people visible at once.", "SELECT * FROM detections WHERE class = 'person'"],
  tracks: ["one row per tracked object", "A track groups the detections of one object in one recording. Use it for distinct counts and durations.", "SELECT * FROM tracks WHERE class = 'person' ORDER BY media_time_us"],
  relations: ["near / holding between objects", "Relations between a subject and an object class. 'near' is a proximity heuristic; 'holding' needs a relation model.", "SELECT * FROM relations WHERE predicate = 'near'"],
  frames: ["one row per analysed frame", "Per-frame counts: people_count, near_phone_count, <class>_count, or count('class').", "SELECT * FROM frames WHERE people_count >= 5"],
  sightings: ["legacy name of detections", "Accepted as an alias of detections by the v1 parser.", "SELECT * FROM sightings LIMIT 10"],
  events: ["not supported yet", "Temporal events are not available yet. Query detections, tracks, relations or frames.", "", "not supported yet"]
};
const T_ = "text", N_ = "num";
const C = (n, t, d, doc) => ({ n, t, d, doc });
const TS = C("track_id", T_, "tracker id", "Identifier of the tracked object (scoped to one recording).");
const COLS = {
  detections: [C("class", T_, "detected class", "Class name such as person, car or cell phone."), C("confidence", N_, "score 0-1", "Detector score, not a probability of being correct."), C("source_id", T_, "camera / source", "The camera or file source."), TS, C("media_time_us", N_, "µs in recording", "Time inside the recording, in microseconds."), C("capture_time_utc_us", N_, "UTC µs", "Wall-clock capture time; empty when the recording has none."), C("model_version", T_, "model id", "Model version that produced the detection."), C("media_id", T_, "recording id", "The recording the row comes from."), C("observation_id", T_, "frame id", "The analysed frame the detection belongs to.")],
  tracks: [C("class", T_, "tracked class", "Class of the tracked object."), C("confidence", N_, "score 0-1", "Track confidence."), C("source_id", T_, "camera / source", "The camera or file source."), TS, C("start_us", N_, "µs", "Media time of the first observation."), C("end_us", N_, "µs", "Media time of the last observation."), C("observations", N_, "count", "Number of distinct frames the track appears in."), C("media_time_us", N_, "µs", "Same as start_us."), C("model_version", T_, "model id", "Model version."), C("media_id", T_, "recording id", "Recording id.")],
  relations: [C("predicate", T_, "near | holding", "Kind of relation: 'near' (proximity) or 'holding'."), C("object_class", T_, "class of the object", "Class of the object, e.g. cell phone."), C("confidence", N_, "score 0-1", "Relation confidence."), C("subject_track_id", T_, "tracker id", "Track of the subject (the person)."), C("object_track_id", T_, "tracker id", "Track of the object."), C("module_version", T_, "relation model", "Relation module version."), C("observation_id", T_, "frame id", "The analysed frame.")]
};
const FRAME_FIXED = [C("people_count", N_, "persons in frame", "Same as count('person')."), C("near_phone_count", N_, "phones near people", "People/phones in a 'near' relation with a cell phone."), C("phone_holders_count", N_, "people holding a phone", "Needs a Holding relation model.", ), C("min_confidence", N_, "frame threshold", "Lowest confidence the frame was analysed with."), C("source_id", T_, "camera / source", "The camera or file source."), C("media_time_us", N_, "µs in recording", "Time inside the recording."), C("capture_time_utc_us", N_, "UTC µs", "Wall-clock capture time."), C("media_id", T_, "recording id", "Recording id.")];
function sqlCols(t) {
  if (t !== "frames") return COLS[t] || COLS.detections;
  const dyn = DATA.labels.filter((l) => l !== "person" && /^[A-Za-z0-9 ]+$/.test(l)).map((l) => C(l.replace(/ /g, "_") + "_count", N_, "count of " + l, `Number of '${l}' objects in the frame (count('${l}')).`));
  return FRAME_FIXED.slice(0, 3).concat(dyn, FRAME_FIXED.slice(3));
}
function colType(t, n) {
  const c = sqlCols(t).find((x) => x.n === n);
  return c ? c.t : /_count$/.test(n) ? N_ : null;
}
const SQL_HINT = { start: "SELECT: begin a query", sel: "SELECT: *, DISTINCT column, COUNT(*) or COUNT(DISTINCT column)", cntArg: "COUNT(: * or DISTINCT column", dcol1: "DISTINCT: pick one column", dcol2: "COUNT(DISTINCT: pick one column", from: "FROM comes next", tbl: "FROM: detections, tracks, relations or frames", tail: "optional WHERE, ORDER BY, LIMIT", w0: "WHERE: conditions on columns of {t}", w1: "operator for the column", w2: "value to compare with", in1: "IN list: quoted strings or numbers", w4: "AND, OR, ORDER BY or LIMIT", ord1: "ORDER BY: media_time_us only", ord2: "ASC or DESC", lim: "LIMIT: an integer" };
const SQL_SNIPS = [
  I("persons above confidence", "snip", "template", "Person detections above a confidence threshold.", { fr: "persons above confidence person", ins: "SELECT * FROM detections\nWHERE class = '${1:person}' AND confidence >= ${2:0.5}\nLIMIT ${3:100}" }),
  I("distinct tracks in a camera", "snip", "template", "How many different people (tracks) one camera saw.", { fr: "distinct tracks per camera source", ins: "SELECT COUNT(DISTINCT track_id) FROM detections\nWHERE class = '${1:person}' AND source_id = '${2:hall}'" }),
  I("frames with N people", "snip", "template", "Analysed frames with at least N people.", { fr: "frames with n people count", ins: "SELECT * FROM frames\nWHERE people_count >= ${1:5}\nORDER BY media_time_us\nLIMIT ${2:100}" }),
  I("phones near people", "snip", "template", "Relations where a cell phone is near a person ('near' is proximity, not holding).", { fr: "phones near people relation", ins: "SELECT * FROM relations\nWHERE predicate = 'near' AND object_class = '${1:cell phone}'\nLIMIT ${2:100}" }),
  I("long person tracks", "snip", "template", "Tracks, newest first.", { fr: "tracks of people recent", ins: "SELECT * FROM tracks\nWHERE class = '${1:person}'\nORDER BY media_time_us DESC\nLIMIT ${2:50}" })
];

/* Lua query API + sandbox library. Verified against the running server (pairs(string/math/table/_G)). */
const L = (s) => s.split("\n").filter(Boolean).map((r) => { const p = r.split(" | "); const g = (i) => (p[i] && p[i] !== "-" ? p[i] : ""); return { n: g(0), k: g(1), d: g(2), doc: g(3), ex: g(4), bad: g(5) }; });
const LUA_KEYWORDS = L(`
for | kw | loop | Numeric or generic loop. | for d in db.detections{} do ... end
in | kw | - | Separates the loop variables from the iterator. | -
do | kw | - | Opens a loop or a plain block. | -
end | kw | - | Closes for, while, if, function and do blocks. | -
if | kw | - | Conditional. | if n > 3 then emit(d) end
then | kw | - | Follows an if/elseif condition. | -
elseif | kw | - | Another condition. | -
else | kw | - | Fallback branch. | -
while | kw | - | Loop while the condition holds. | -
repeat | kw | - | Loop until the condition holds. | -
until | kw | - | Ends repeat. | -
function | kw | - | Defines a function. | local function ratio(a, b) return a / b end
local | kw | - | Declares a local variable. | local n = 0
return | kw | - | Returns a value; it appears as the query's values. | return {count = n}
break | kw | - | Leaves the innermost loop early. | -
and | kw | - | Logical and. | -
or | kw | - | Logical or. | -
not | kw | - | Logical not. | -
nil | kw | - | No value. Unknown counts are nil, never 0. | -
true | kw | - | Boolean true. | -
false | kw | - | Boolean false. | -
`);
const LUA_GLOBALS = L(`
db | mod | query tables | db.detections, db.frames, db.relations, db.tracks: each returns an iterator over rows. | for d in db.detections{class = 'person'} do emit(d) end
emit | fn | emit(row or observation_id) | Adds evidence: a review window around the row. Emit a row you have already read. | emit(d)
collect | fn | collect(iterator) -> table | Reads every row of an iterator into a table. | local rows = collect(db.detections{class = 'car'})
count | fn | count(iterator or table) -> number | Number of rows. | count(rows)
distinct | fn | distinct(iterator or table, key) -> number | Number of distinct values of a key (track_id is scoped per media). | distinct(rows, 'track_id')
pairs | fn | pairs(t) | Iterates all key/value pairs. | for k, v in pairs(t) do end
ipairs | fn | ipairs(t) | Iterates 1..n of a list. | for i, r in ipairs(rows) do end
next | fn | next(t [, k]) | Next key/value of a table. | -
select | fn | select(n, ...) | Returns arguments from n on; select('#', ...) counts them. | -
type | fn | type(v) -> string | 'nil', 'number', 'string', 'boolean', 'table', 'function'. | type(f.counts.person) == 'number'
tostring | fn | tostring(v) | Converts to a string. | -
tonumber | fn | tonumber(v [, base]) | Converts to a number or nil. | -
pcall | fn | pcall(f, ...) -> ok, result | Calls f and catches errors. | local ok, err = pcall(function() end)
xpcall | fn | xpcall(f, handler, ...) | pcall with an error handler. | -
error | fn | error(message [, level]) | Raises an error (the query ends with runtime_error). | -
assert | fn | assert(v [, message]) | Raises an error when v is false or nil. | -
getmetatable | fn | getmetatable(t) | The metatable of a table. | -
setmetatable | fn | setmetatable(t, mt) | Sets a table's metatable. | -
rawget | fn | rawget(t, k) | Reads without metamethods. | -
rawset | fn | rawset(t, k, v) | Writes without metamethods. | -
rawequal | fn | rawequal(a, b) | Compares without metamethods. | -
rawlen | fn | rawlen(v) | Length without metamethods. | -
warn | fn | warn(msg) | Emits a warning. | -
string | mod | string library | find, format, gsub, lower, sub, ... | -
math | mod | math library | floor, max, min, abs, huge, ... | -
table | mod | table library | insert, remove, sort, concat, ... | -
_G | var | globals table | The global environment. | -
_VERSION | var | "Lua 5.4" | Interpreter version string. | -
`);
const LUA_BAD = L(`
io | mod | - | - | - | not available in the query sandbox
os | mod | - | - | - | not available in the query sandbox
package | mod | - | - | - | not available in the query sandbox
debug | mod | - | - | - | not available in the query sandbox
require | fn | - | - | - | not available in the query sandbox
load | fn | - | - | - | not available in the query sandbox
loadstring | fn | - | - | - | not available in the query sandbox
loadfile | fn | - | - | - | not available in the query sandbox
dofile | fn | - | - | - | not available in the query sandbox
print | fn | - | - | - | not available (use return or emit)
collectgarbage | fn | - | - | - | not available in the query sandbox
`);
const LIBS = {
  string: L(`
byte | fn | string.byte(s [, i [, j]]) | Numeric codes of characters. | -
char | fn | string.char(...) | String from character codes. | -
find | fn | string.find(s, pattern [, init [, plain]]) | Position of a match, or nil. | string.find(d.class, 'cell')
format | fn | string.format(fmt, ...) | printf-style formatting. | string.format('%.2f', d.confidence)
gmatch | fn | string.gmatch(s, pattern) | Iterator over matches. | -
gsub | fn | string.gsub(s, pattern, repl) | Replace matches. | -
len | fn | string.len(s) | Length of the string. | -
lower | fn | string.lower(s) | Lower-case copy. | -
match | fn | string.match(s, pattern) | First match or captures. | -
pack | fn | string.pack(fmt, ...) | Binary packing. | -
packsize | fn | string.packsize(fmt) | Size of a packed format. | -
rep | fn | string.rep(s, n) | Repeat a string. | -
reverse | fn | string.reverse(s) | Reversed copy. | -
sub | fn | string.sub(s, i [, j]) | Substring. | -
unpack | fn | string.unpack(fmt, s) | Binary unpacking. | -
upper | fn | string.upper(s) | Upper-case copy. | -
dump | fn | - | - | - | not available in the query sandbox
`),
  math: L(`
abs | fn | math.abs(x) | Absolute value. | -
acos | fn | math.acos(x) | Arc cosine. | -
asin | fn | math.asin(x) | Arc sine. | -
atan | fn | math.atan(y [, x]) | Arc tangent. | -
atan2 | fn | math.atan2(y, x) | Arc tangent of y/x. | -
ceil | fn | math.ceil(x) | Round up. | -
cos | fn | math.cos(x) | Cosine. | -
cosh | fn | math.cosh(x) | Hyperbolic cosine. | -
deg | fn | math.deg(x) | Radians to degrees. | -
exp | fn | math.exp(x) | e^x. | -
floor | fn | math.floor(x) | Round down. | math.floor(d.media_time_us / 1000000)
fmod | fn | math.fmod(x, y) | Remainder of x / y. | -
frexp | fn | math.frexp(x) | Mantissa and exponent. | -
huge | var | math.huge | Positive infinity. | local best = math.huge
ldexp | fn | math.ldexp(m, e) | m * 2^e. | -
log | fn | math.log(x [, base]) | Logarithm. | -
log10 | fn | math.log10(x) | Base-10 logarithm. | -
max | fn | math.max(x, ...) | Largest argument. | -
maxinteger | var | math.maxinteger | Largest integer. | -
min | fn | math.min(x, ...) | Smallest argument. | -
mininteger | var | math.mininteger | Smallest integer. | -
modf | fn | math.modf(x) | Integer and fractional parts. | -
pi | var | math.pi | The constant pi. | -
pow | fn | math.pow(x, y) | x^y. | -
rad | fn | math.rad(x) | Degrees to radians. | -
random | fn | math.random([m [, n]]) | Pseudo-random number (not repeatable between runs). | -
randomseed | fn | math.randomseed(x) | Seeds math.random. | -
sin | fn | math.sin(x) | Sine. | -
sinh | fn | math.sinh(x) | Hyperbolic sine. | -
sqrt | fn | math.sqrt(x) | Square root. | -
tan | fn | math.tan(x) | Tangent. | -
tanh | fn | math.tanh(x) | Hyperbolic tangent. | -
tointeger | fn | math.tointeger(x) | Integer value or nil. | -
type | fn | math.type(x) | 'integer', 'float' or nil. | -
ult | fn | math.ult(m, n) | Unsigned integer comparison. | -
`),
  table: L(`
concat | fn | table.concat(t [, sep]) | Joins list items into a string. | table.concat(ids, ', ')
insert | fn | table.insert(t, [pos,] v) | Appends or inserts a value. | table.insert(rows, d)
move | fn | table.move(a, f, e, t) | Moves elements. | -
pack | fn | table.pack(...) | Packs arguments into a table with n. | -
remove | fn | table.remove(t [, pos]) | Removes and returns an element. | -
sort | fn | table.sort(t [, cmp]) | Sorts a list in place. | table.sort(rows, function(a, b) return a.confidence > b.confidence end)
unpack | fn | table.unpack(t [, i [, j]]) | Returns list items as values. | -
`)
};
const LUA_DB = L(`
detections | fn | db.detections{...} | One row per detected object. Rows: observation_id detection_id source_id media_id class confidence model_version track_id media_time_us capture_time_utc_us. | for d in db.detections{class = 'person', min_confidence = 0.5} do emit(d) end
frames | fn | db.frames{...} | One row per analysed frame with counts and relations tables. | for f in db.frames{} do if (f.counts.person or 0) >= 5 then emit(f) end end
relations | fn | db.relations{predicate = 'near'} | Relations between objects. 'holding' fails with unsupported without a Holding model. | for r in db.relations{predicate = 'near'} do emit(r) end
tracks | fn | db.tracks{...} | One row per tracked object with start_us, end_us and observations. | for t in db.tracks{class = 'person'} do emit(t) end
`);
const DBKEYS = L(`
class | val | string | Only detections/tracks of this class. | db.detections{class = 'person'}
classes | val | {strings} | Any of these classes. | db.detections{classes = {'car', 'bus'}}
min_confidence | val | number 0-1 | Minimum confidence. | db.detections{min_confidence = 0.6}
source_ids | val | {strings} | Only these sources (cameras). | db.detections{source_ids = {'hall'}}
media_ids | val | {strings} | Only these recordings. | -
model_version | val | string | Only this model version. | -
track_id | val | string | Only this track. | -
from_us | val | number | Media time lower bound, microseconds. | db.detections{from_us = 10000000}
to_us | val | number | Media time upper bound, microseconds. | -
predicate | val | 'near' or 'holding' | Relation kind (db.relations only). | db.relations{predicate = 'near'}
`);
const ROWF = {
  detections: [["class", "string"], ["confidence", "number"], ["source_id", "string"], ["track_id", "string | nil"], ["media_time_us", "number"], ["capture_time_utc_us", "number | nil"], ["model_version", "string"], ["media_id", "string"], ["observation_id", "string"], ["detection_id", "string"]],
  frames: [["counts", "table: class -> number | nil"], ["relations", "table: 'predicate:class' -> number | nil"], ["media_time_us", "number"], ["source_id", "string"], ["min_confidence", "number"], ["media_id", "string"], ["observation_id", "string"], ["capture_time_utc_us", "number | nil"]],
  relations: [["predicate", "string"], ["object_class", "string"], ["confidence", "number"], ["track_id", "string | nil"], ["subject_track_id", "string | nil"], ["object_track_id", "string | nil"], ["observation_id", "string"], ["detection_id", "string"], ["relation_id", "string"], ["module_version", "string"]],
  tracks: [["class", "string"], ["track_id", "string"], ["start_us", "number"], ["end_us", "number"], ["observations", "number"], ["confidence", "number"], ["source_id", "string"], ["media_time_us", "number"], ["model_version", "string"], ["media_id", "string"], ["key", "string"]]
};
const ROWDOC = { counts: "Counts per class in this frame. counts.person is nil when the class was not analysed (unknown is never 0).", relations: "Counts per 'predicate:class', e.g. relations['near:cell phone']. nil when unsupported.", class: "Class name.", confidence: "Detector score 0-1.", track_id: "Tracker id, nil without tracking.", media_time_us: "Microseconds from the start of the recording.", start_us: "Media time of the first observation.", end_us: "Media time of the last observation.", observations: "Number of distinct frames in the track.", predicate: "'near' or 'holding'.", object_class: "Class of the relation's object." };
const LUA_SNIPS = [
  I("for each detection", "snip", "template", "Loop over detections of a class and emit them as evidence.", { fr: "for loop detections each", ins: "for d in db.detections{class = '${1:person}', min_confidence = ${2:0.5}} do\n  emit(d)\nend" }),
  I("for each frame", "snip", "template", "Loop over frames; counts.<class> is nil when unknown.", { fr: "for loop frames each", ins: "for f in db.frames{} do\n  local p = f.counts.${1:person}\n  if p and p >= ${2:5} then emit(f) end\nend" }),
  I("nested loop (people x phones)", "snip", "template", "Pair people with phones that share a frame.", { fr: "nested loop people phones", ins: "local phones = collect(db.detections{class = 'cell phone', min_confidence = 0.5})\nfor p in db.detections{class = 'person'} do\n  for _, ph in ipairs(phones) do\n    if ph.observation_id == p.observation_id then emit(p) emit(ph) end\n  end\nend" }),
  I("per-frame ratio (near)", "snip", "template", "Frames where a share of people have a phone near them.", { fr: "per frame ratio near phone", ins: "for f in db.frames{} do\n  local p, h = f.counts.person, f.relations['near:cell phone']\n  if p and h and p > 0 and h / p >= ${1:0.2} then emit(f) end\nend" }),
  I("aggregate with return", "snip", "template", "Count and distinct values, returned as values.", { fr: "aggregate return count distinct", ins: "local rows = collect(db.detections{class = '${1:person}'})\nreturn {observations = count(rows), tracks = distinct(rows, 'track_id')}" }),
  I("early break", "snip", "template", "Stop after the first N rows.", { fr: "early break limit", ins: "local n = 0\nfor d in db.detections{class = '${1:person}'} do\n  emit(d)\n  n = n + 1\n  if n >= ${2:20} then break end\nend" }),
  I("long tracks", "snip", "template", "Tracks longer than a number of microseconds.", { fr: "long tracks duration", ins: "local n = 0\nfor t in db.tracks{class = '${1:person}'} do\n  if t.end_us - t.start_us > ${2:10000000} then n = n + 1 emit(t) end\nend\nreturn {long_tracks = n}" })
];
const SIGS = {
  "db.detections": { args: ["class", "classes", "min_confidence", "source_ids", "media_ids", "model_version", "track_id", "from_us", "to_us"], brace: 1, doc: "Detection rows." },
  "db.frames": { args: ["class", "classes", "min_confidence", "source_ids", "media_ids", "model_version", "track_id", "from_us", "to_us"], brace: 1, doc: "Frame rows (counts, relations)." },
  "db.relations": { args: ["predicate", "class", "classes", "min_confidence", "source_ids", "media_ids", "model_version", "track_id", "from_us", "to_us"], brace: 1, doc: "Relation rows; predicate 'near' | 'holding'." },
  "db.tracks": { args: ["class", "classes", "min_confidence", "source_ids", "media_ids", "model_version", "track_id", "from_us", "to_us"], brace: 1, doc: "Track rows." },
  emit: { args: ["row | observation_id"], doc: "Adds evidence; emit a row you have already read." },
  collect: { args: ["iterator"], ret: "table", doc: "Reads all rows into a table." },
  count: { args: ["iterator | table"], ret: "number", doc: "Number of rows." },
  distinct: { args: ["iterator | table", "key"], ret: "number", doc: "Distinct values of key (track_id is scoped per media)." },
  ipairs: { args: ["t"], doc: "Iterate a list." }, pairs: { args: ["t"], doc: "Iterate key/value pairs." },
  "string.format": { args: ["fmt", "..."], ret: "string", doc: "printf-style formatting." }, "string.find": { args: ["s", "pattern", "init", "plain"], doc: "Position of a match." }, "math.floor": { args: ["x"], doc: "Round down." }, "math.max": { args: ["x", "..."], doc: "Largest argument." }, "math.min": { args: ["x", "..."], doc: "Smallest argument." }, "table.insert": { args: ["t", "pos", "v"], doc: "Append or insert." }, "table.sort": { args: ["t", "cmp"], doc: "Sort in place." }, "table.concat": { args: ["t", "sep"], ret: "string", doc: "Join list items." },
  "ctx:count": { args: ["class"], ret: "number | nil", doc: "Distinct objects of the class, nil when unknown." }, "ctx:count_relation": { args: ["predicate", "class"], ret: "number | nil", doc: "Distinct subjects of 'predicate:class'." }, "ctx:ratio_people_with": { args: ["predicate", "class"], ret: "number | nil", doc: "Subjects / people in the same observation." },
  seen: { args: ['"class"'], ret: "boolean", doc: "True when the class is visible in the frame." },
  ratio: { args: ['"numerator class"', '"denominator class"'], doc: "Count ratio, then a comparison and a number." }
};
const RULE_CTX = L(`
count | fn | ctx:count(class) -> number or nil | Distinct objects of the class, or nil when unknown/unsupported. | ctx:count('cell phone') >= 4 | typed Lua rules only (not this one-line editor)
count_people | fn | ctx:count_people() | Same as count('person'). | ctx:count_people() >= 50 | typed Lua rules only (not this one-line editor)
count_relation | fn | ctx:count_relation(pred, class) | Distinct subjects of 'holding' or 'near' with the class. | ctx:count_relation('near', 'cell phone') | typed Lua rules only (not this one-line editor)
ratio_people_with | fn | ctx:ratio_people_with(pred, class) | Subjects / people in the same observation. | ctx:ratio_people_with('near', 'cell phone') >= 0.2 | typed Lua rules only (not this one-line editor)
source_id | var | ctx.source_id | Source of the frame. | - | typed Lua rules only (not this one-line editor)
zone | var | ctx.zone | Rule zone. | - | typed Lua rules only (not this one-line editor)
media_time_us | var | ctx.media_time_us | Media time of the frame. | - | typed Lua rules only (not this one-line editor)
`);
const RULE_OPS = [[">=", "at least"], [">", "more than"], ["<=", "at most"], ["<", "fewer than"], ["==", "exactly"]];

/* ---- providers: (text, caret, opts) -> {items, from, to, prefix, str, hint} | null ---- */
const q1 = (s) => "'" + String(s).replace(/'/g, "''") + "'";
const wordEnd = (t, i) => { while (i < t.length && /\w/.test(t[i])) i++; return i; };
const strEnd = (t, i, q) => { let j = i; while (j < t.length && t[j] !== "\n" && t[j] !== q) j++; return j; };
const SQL_RX = /--[^\n]*|'(?:[^']|'')*'?|\d+(?:\.\d+)?|[A-Za-z_]\w*|[<>!=]=?|[*\/(),;]/g;
function sqlTok(s) {
  const out = []; let m; SQL_RX.lastIndex = 0;
  while ((m = SQL_RX.exec(s))) { const v = m[0], c = v[0]; if (c === "-") continue; out.push({ t: c === "'" ? "str" : /\d/.test(c) ? "num" : /[A-Za-z_]/.test(c) ? "word" : /[<>!=]/.test(c) ? "op" : "sym", v, a: m.index, b: m.index + v.length }); }
  return out;
}
const kwItem = (name, extra) => { const k = SQL_DOC[name]; return I(name, "kw", k[0], k[1], Object.assign({ ex: k[2], ins: name + " " }, extra)); };
function sqlValues(col, tbl, str) {
  const wrap = (v) => (str ? String(v).replace(/'/g, "''") : q1(v)), out = [];
  if (col === "class" || col === "object_class") DATA.labels.forEach((l) => out.push(I(l, "cls", "class", `Detected class '${l}'.`, { ins: wrap(l), fr: l })));
  else if (col === "source_id") DATA.sources.forEach((s) => out.push(I(s.id, "src", s.name === s.id ? "source" : s.name, `Source '${s.id}'.`, { ins: wrap(s.id) })));
  else if (col === "model_version" || col === "module_version") DATA.versions.forEach((v) => out.push(I(v, "val", "installed model", "Model id installed now; older stored versions have other ids.", { ins: wrap(v) })));
  else if (col === "predicate") { out.push(I("near", "val", "proximity heuristic", "Subject and object are close in the frame; does not prove holding.", { ins: wrap("near") })); out.push(I("holding", "val", holdBad() ? "unsupported" : "relation", "A person holds the object.", { ins: wrap("holding"), bad: holdBad() })); }
  else if (!str && (col === "confidence" || col === "min_confidence")) [["0.5", "balanced"], ["0.6", "confident"], ["0.8", "very confident"], ["0.35", "model default"]].forEach(([v, d]) => out.push(I(v, "val", d, "Confidence threshold (0-1).", { ins: v })));
  return out;
}
function sqlComplete(text, c, o) {
  const toks = sqlTok(text), strClosed = (v) => /^'(?:[^']|'')*'$/.test(v), tk = toks.find((t) => t.a < c && c <= t.b && (t.t === "word" || t.t === "num" || (t.t === "str" && (c < t.b || !strClosed(t.v)))));
  let from = c, to = c, prefix = "", str = false, closed = false, ctxPrev;
  if (tk && tk.t === "str") { str = true; from = tk.a + 1; prefix = text.slice(from, c); closed = strClosed(tk.v); to = closed ? tk.b - 1 : tk.b; ctxPrev = toks.filter((t) => t.b <= tk.a); }
  else if (tk && tk.t === "word") { from = tk.a; to = wordEnd(text, c); prefix = text.slice(tk.a, c); ctxPrev = toks.filter((t) => t.b <= tk.a); }
  else if (tk) return null;
  else ctxPrev = toks.filter((t) => t.b <= c);
  const ft = toks.findIndex((t) => t.t === "word" && t.v.toUpperCase() === "FROM"), tn = ft >= 0 && toks[ft + 1] && toks[ft + 1].t === "word" ? toks[ft + 1].v.toLowerCase() : "";
  const legacy = DATA.legacy, tbl = SQL_TABLES[tn] && tn !== "events" ? (tn === "sightings" ? "detections" : tn) : "detections";
  // state machine over the tokens before the word being typed
  let st = "start", col = null, lt = null, fn = null, depth = 0;
  for (const t of ctxPrev) {
    const u = t.v.toUpperCase(), isw = t.t === "word", lit = t.t === "str" || t.t === "num";
    switch (st) {
      case "start": st = u === "SELECT" ? "sel" : "bad"; break;
      case "sel": st = t.v === "*" ? "from" : u === "DISTINCT" ? "dcol1" : u === "COUNT" ? "cnt" : "bad"; break;
      case "cnt": st = t.v === "(" ? "cntArg" : "bad"; break;
      case "cntArg": st = t.v === "*" ? "cclose" : u === "DISTINCT" ? "dcol2" : "bad"; break;
      case "dcol1": st = isw ? "from" : "bad"; break;
      case "dcol2": st = isw ? "cclose" : "bad"; break;
      case "cclose": st = t.v === ")" ? "from" : "bad"; break;
      case "from": st = u === "FROM" ? "tbl" : "bad"; break;
      case "tbl": st = isw ? "tail" : "bad"; break;
      case "tail": st = u === "WHERE" ? "w0" : u === "ORDER" ? "ord0" : u === "LIMIT" ? "lim" : t.v === ";" ? "end" : "bad"; break;
      case "w0": if (u === "NOT") break; if (t.v === "(") { depth++; break; } if (isw && u === "COUNT") { st = "fc0"; break; } if (isw) { col = t.v.toLowerCase(); lt = colType(tbl, col); st = "w1"; } else if (lit) { col = null; lt = t.t === "str" ? T_ : N_; st = "w1"; } else st = "bad"; break;
      case "fc0": st = t.v === "(" ? "fc1" : "bad"; break;
      case "fc1": st = t.t === "str" ? "fc2" : "bad"; break;
      case "fc2": if (t.v === ")") { col = null; lt = N_; st = "w1"; } else st = "bad"; break;
      case "w1": if (u === "IN") st = "in0"; else if (u === "NOT") st = "w1n"; else if (t.t === "op") st = "w2"; else if (t.v === "*" || t.v === "/") { st = "w0"; } else st = "bad"; break;
      case "w1n": st = u === "IN" ? "in0" : "bad"; break;
      case "in0": st = t.v === "(" ? "in1" : "bad"; break;
      case "in1": st = lit ? "in2" : "bad"; break;
      case "in2": st = t.v === "," ? "in1" : t.v === ")" ? "w4" : "bad"; break;
      case "w2": st = lit || isw ? "w4" : "bad"; break;
      case "w4": if (u === "AND" || u === "OR") { st = "w0"; col = null; } else if (t.v === ")") depth--; else if (t.v === "*" || t.v === "/") st = "w2"; else if (u === "ORDER") st = "ord0"; else if (u === "LIMIT") st = "lim"; else if (t.v === ";") st = "end"; else st = "bad"; break;
      case "ord0": st = u === "BY" ? "ord1" : "bad"; break;
      case "ord1": st = isw ? "ord2" : "bad"; break;
      case "ord2": st = u === "ASC" || u === "DESC" ? "ord3" : u === "LIMIT" ? "lim" : "bad"; break;
      case "ord3": st = u === "LIMIT" ? "lim" : "bad"; break;
      case "lim": st = t.t === "num" ? "end" : "bad"; break;
      default: st = "bad";
    }
  }
  const out = [], hint = (SQL_HINT[st] || "").replace("{t}", tbl);
  const cols = (types) => sqlCols(tbl).filter((x) => !types || x.t === types).map((x) => I(x.n, "col", x.t === T_ ? "text · " + x.d : "number · " + x.d, x.doc, { bad: x.n === "phone_holders_count" ? holdBad() : "", ins: x.n + " " }));
  const valuesNow = () => { if (st === "fc1") return DATA.labels.map((l) => I(l, "cls", "class", `count('${l}')`, { ins: str ? l.replace(/'/g, "''") : q1(l) })); return sqlValues(col, tbl, str); };
  if (legacy) { return legacyComplete(text, c, { from, to, prefix, str, closed, st, hint }); }
  if (str) { if (st === "w2" || st === "in1" || st === "fc1") out.push(...valuesNow()); if (!out.length) return null; return { items: out, from, to, prefix, str, closed, hint }; }
  switch (st) {
    case "start": out.push(kwItem("SELECT"), ...SQL_SNIPS); break;
    case "sel": out.push(I("*", "op", "all columns", "Every column of the table.", { ins: "* " }), kwItem("DISTINCT"), I("COUNT(*)", "fn", "row count", "Number of rows.", { ins: "COUNT(*) ", ex: "SELECT COUNT(*) FROM detections" }), I("COUNT(DISTINCT …)", "fn", "unique values", SQL_DOC.COUNT[1], { ins: "COUNT(DISTINCT ${1:track_id}) ", fr: "COUNT(DISTINCT" })); break;
    case "cntArg": out.push(I("*", "op", "rows", "COUNT(*)", { ins: "*)" }), kwItem("DISTINCT")); break;
    case "dcol1": case "dcol2": out.push(...cols()); break;
    case "cclose": out.push(I(")", "op", "", "Closes COUNT(", { ins: ") " })); break;
    case "from": out.push(kwItem("FROM")); break;
    case "tbl": Object.keys(SQL_TABLES).forEach((n) => { const t = SQL_TABLES[n]; out.push(I(n, "tbl", t[0], t[1], { ex: t[2], bad: t[3] || "", ins: n + " " })); }); break;
    case "tail": out.push(kwItem("WHERE"), kwItem("ORDER BY", tbl === "relations" ? { bad: "ORDER BY is not supported on relations" } : {}), kwItem("LIMIT")); break;
    case "w0": out.push(...cols()); if (tbl === "frames") out.push(I("count('class')", "fn", "per-frame count", "Number of objects of a class in the frame (frames table only).", { ins: "count('${1:person}')", fr: "count", ex: "WHERE count('person') >= 5" })); out.push(kwItem("NOT"), I("(", "op", "group", "Parenthesised condition.", { ins: "(" })); break;
    case "w1": {
      const ops = [["=", "equals"], ["!=", "not equal"], ["<", "less than"], ["<=", "at most"], [">", "greater than"], [">=", "at least"]].filter((o) => lt === N_ || o[0] === "=" || o[0] === "!=");
      ops.forEach((o) => out.push(I(o[0], "op", o[1], `Compares the ${lt === T_ ? "text" : "number"} column: ${o[1]}.`, { ins: o[0] + " " })));
      out.push(kwItem("IN"), kwItem("NOT IN")); if (lt === N_) out.push(I("*", "op", "multiply", "Arithmetic on numbers.", { ins: "* " }), I("/", "op", "divide", "Arithmetic on numbers.", { ins: "/ " })); break;
    }
    case "w2": if (col) out.push(...sqlValues(col, tbl, false)); if (lt === N_ && !out.length) [["1", ""], ["5", ""], ["10", ""]].forEach(([v]) => out.push(I(v, "val", "number", "Number literal.", { ins: v }))); out.push(...cols().filter((x) => (colType(tbl, x.l) === lt))); break;
    case "in0": out.push(I("(", "op", "list", "Start of the IN list.", { ins: "(" })); break;
    case "in1": if (col) out.push(...sqlValues(col, tbl, false)); break;
    case "in2": out.push(I(",", "op", "next value", "Another value.", { ins: ", " }), I(")", "op", "end of list", "Closes the IN list.", { ins: ") " })); break;
    case "w4": out.push(kwItem("AND"), kwItem("OR"), kwItem("ORDER BY", tbl === "relations" ? { bad: "ORDER BY is not supported on relations" } : {}), kwItem("LIMIT")); break;
    case "ord0": out.push(I("BY", "kw", "", "ORDER BY media_time_us", { ins: "BY " })); break;
    case "ord1": out.push(I("media_time_us", "col", "number · µs", "The only sortable column.", { ins: "media_time_us ", bad: tbl === "relations" ? "not supported on relations" : "" })); break;
    case "ord2": out.push(kwItem("ASC"), kwItem("DESC"), kwItem("LIMIT")); break;
    case "ord3": out.push(kwItem("LIMIT")); break;
    case "lim": [10, 50, 100, 500, 1000].forEach((n) => out.push(I(String(n), "val", "rows", "Maximum number of rows.", { ins: String(n) }))); break;
    default: if (st === "bad" && !prefix) return null;
  }
  return out.length ? { items: out, from, to, prefix, str, closed, hint } : { items: [], from, to, prefix, hint };
}
// Legacy `sightings` grammar (rust/src/watch/query.rs): SELECT *|id|COUNT(*) FROM sightings [WHERE ...] [GROUP BY camera_id] [LIMIT n]
const LEG_COLS = ["id", "ts", "at", "camera", "camera_id", "class", "color", "confidence", "track_id", "model", "near_person"];
function legacyComplete(text, c, p) {
  const out = [], b = text.slice(0, p.from).trimEnd(), last = (b.match(/(\w+|\W)$/) || [""])[0].toUpperCase();
  if (p.str) return null;
  if (!b) out.push(kwItem("SELECT"));
  else if (/SELECT$/i.test(b)) out.push(I("*", "op", "", "All columns", { ins: "* " }), I("COUNT(*)", "fn", "", "Row count", { ins: "COUNT(*) " }));
  else if (/(\*|\)|\bid)$/i.test(b) && !/\bFROM\b/i.test(b)) out.push(kwItem("FROM"));
  else if (/FROM$/i.test(b)) out.push(I("sightings", "tbl", "legacy log", "The classic sighting log.", { ins: "sightings " }));
  else if (/(WHERE|AND|OR)$/i.test(b)) LEG_COLS.forEach((n) => out.push(I(n, "col", "", "Legacy sighting column.", { ins: n })));
  else out.push(kwItem("WHERE"), kwItem("AND"), kwItem("OR"), I("GROUP BY camera_id", "kw", "", "Count per camera.", { ins: "GROUP BY camera_id " }), kwItem("LIMIT"));
  void last; return { items: out, from: p.from, to: p.to, prefix: p.prefix, hint: "classic sighting query" };
}

/* ---- Lua ---- */
const LUA_RXS = /--\[(=*)\[[\s\S]*?(?:\]\1\]|$)|--[^\n]*|\[(=*)\[[\s\S]*?(?:\]\2\]|$)|"(?:[^"\\\n]|\\.)*"?|'(?:[^'\\\n]|\\.)*'?|\d+(?:\.\d+)?|[A-Za-z_]\w*|\.\.\.|\.\.|==|~=|<=|>=|::|[^\s]/g;
function luaTok(s) {
  const out = []; const re = new RegExp(LUA_RXS.source, "g"); let m;
  while ((m = re.exec(s))) { const v = m[0], c = v[0]; out.push({ t: c === "-" && v[1] === "-" ? "com" : c === '"' || c === "'" || (c === "[" && /^\[=*\[/.test(v)) ? "str" : /\d/.test(c) ? "num" : /[A-Za-z_]/.test(c) ? "word" : "sym", v, a: m.index, b: m.index + v.length }); }
  return out;
}
const unterminated = (t) => {
  if (t.t === "com") return /^--\[=*\[/.test(t.v) ? !/\]=*\]$/.test(t.v) || t.v.length < 6 : true;
  if (t.v[0] === "[") return !/\]=*\]$/.test(t.v);
  return !(t.v.length >= 2 && t.v.endsWith(t.v[0]) && /^(?:[^\\]|\\.)*$/.test(t.v.slice(1, -1)));
};
// scope: loop variables bound to db tables, collected arrays, locals
function luaScope(toks) {
  const blocks = [], arr = {}, locals = new Set(); let i = 0;
  const iterTable = (h, k) => { for (let j = k; j < h.length - 2; j++) if (h[j].v === "db" && h[j + 1].v === "." && h[j + 2].t === "word") return h[j + 2].v; return null; };
  for (; i < toks.length; i++) {
    const t = toks[i]; if (t.t !== "word") continue; const v = t.v;
    if (v === "for" || v === "while") {
      let j = i + 1; while (j < toks.length && !(toks[j].t === "word" && toks[j].v === "do")) j++;
      const h = toks.slice(i + 1, j), b = {};
      if (v === "for") {
        const inn = h.findIndex((x) => x.v === "in"), names = h.slice(0, inn < 0 ? 0 : inn).filter((x) => x.t === "word").map((x) => x.v);
        names.forEach((n) => locals.add(n));
        if (inn >= 0) {
          const tb = iterTable(h, inn + 1);
          if (tb && names.length === 1) b[names[0]] = tb;
          else if (/^(ipairs|pairs)$/.test((h[inn + 1] || {}).v || "") && names.length === 2) { const a = h[inn + 3]; const t2 = a && (arr[a.v] || (a.v === "collect" ? iterTable(h, inn + 3) : null)); if (t2) b[names[1]] = t2; }
        }
      }
      blocks.push(b); i = Math.min(j, toks.length);
    } else if (v === "if" || v === "function" || v === "do" || v === "repeat") { blocks.push({}); if (v === "function") { let k = i + 1; if (toks[k] && toks[k].t === "word") { locals.add(toks[k].v); k++; } if (toks[k] && toks[k].v === "(") for (k++; k < toks.length && toks[k].v !== ")"; k++) if (toks[k].t === "word") locals.add(toks[k].v); } }
    else if (v === "end" || v === "until") blocks.pop();
    else if (v === "local") {
      let k = i + 1; if (toks[k] && toks[k].v === "function") continue; const names = [];
      while (toks[k] && (toks[k].t === "word" || toks[k].v === ",")) { if (toks[k].t === "word") names.push(toks[k].v); k++; }
      names.forEach((n) => locals.add(n));
      if (names.length === 1 && toks[k] && toks[k].v === "=" && toks[k + 1] && toks[k + 1].v === "collect") { const tb = iterTable(toks.slice(k, k + 8), 0); if (tb) arr[names[0]] = tb; }
    }
  }
  const vars = {}; blocks.forEach((b) => Object.assign(vars, b));
  return { vars, locals, arr };
}
function luaChain(prev) {
  const chain = []; let i = prev.length - 1; // prev ends with '.' or ':'
  while (i >= 1 && (prev[i].v === "." || prev[i].v === ":") && prev[i - 1].t === "word") { chain.unshift(prev[i - 1].v); i -= 2; }
  return chain;
}
const fieldItems = (tb) => (ROWF[tb] || []).map(([n, ty], i) => I(n, "col", ty, ROWDOC[n] || `${n} of a ${tb} row.`, { s: i }));
const lib = (rows, prefix) => rows.map((r) => I(r.n, r.k === "var" ? "var" : "fn", r.d.replace(new RegExp("^" + prefix + "[.:]"), ""), r.doc, { sig: r.d, ex: r.ex, bad: r.bad }));
const relKeys = () => { const o = []; ["near", "holding"].forEach((p) => DATA.labels.forEach((l) => o.push({ p, l }))); return o; };
function luaValues(key, str, inList) {
  const w = (v) => (str ? v : `'${v}'`), out = [];
  const lst = (items) => (inList || str ? items : items.map((it) => Object.assign(it, { ins: "{" + it.ins + "}" })));
  if (key === "class" || key === "classes") { const it = DATA.labels.map((l) => I(l, "cls", "class", `Detected class '${l}'.`, { ins: w(l) })); out.push(...(key === "classes" ? lst(it) : it)); }
  else if (key === "source_ids") out.push(...lst(DATA.sources.map((s) => I(s.id, "src", s.name === s.id ? "source" : s.name, `Source '${s.id}'.`, { ins: w(s.id) }))));
  else if (key === "model_version") DATA.versions.forEach((v) => out.push(I(v, "val", "installed model", "Model id installed now.", { ins: w(v) })));
  else if (key === "predicate") { out.push(I("near", "val", "proximity heuristic", "Spatial proximity; not proof of holding.", { ins: w("near") })); out.push(I("holding", "val", holdBad() ? "unsupported" : "relation", "A person holds the object.", { ins: w("holding"), bad: holdBad() })); }
  else if (key === "min_confidence" && !str) [["0.5", "balanced"], ["0.6", "confident"], ["0.8", "very confident"], ["0.35", "model default"]].forEach(([v, d]) => out.push(I(v, "val", d, "Confidence threshold (0-1).", { ins: v })));
  return out;
}
const CALLS = { count: "class", seen: "class", "ctx:count": "class", "ctx:count_relation": ["pred", "class"], "ctx:ratio_people_with": ["pred", "class"] };
function luaComplete(text, c, o) {
  const toks = luaTok(text.slice(0, c)), last = toks[toks.length - 1], prof = o.prof;
  let from = c, to = c, prefix = "", str = false, closed = false, prev = toks;
  if (last && last.b === c) {
    if (last.t === "com" && unterminated(last)) return null;
    if (last.t === "str" && unterminated(last)) { str = true; from = last.a + 1; prefix = text.slice(from, c); to = strEnd(text, c, last.v[0]); closed = text[to] === last.v[0]; prev = toks.slice(0, -1); }
    else if (last.t === "word") { from = last.a; prefix = last.v; to = wordEnd(text, c); prev = toks.slice(0, -1); }
    else if (last.t === "num") return null;
  }
  const stack = []; prev.forEach((t, i) => { if (t.t !== "sym") return; if ("([{".includes(t.v)) stack.push({ v: t.v, i }); else if (")]}".includes(t.v)) stack.pop(); });
  const top = stack[stack.length - 1], pt = prev[prev.length - 1], pt2 = prev[prev.length - 2];
  const sc = luaScope(prev), tb = (i) => prev[i] && prev[i - 1] && prev[i - 2] && prev[i - 2].v === "db" && prev[i - 1].v === "." && prev[i].t === "word" ? prev[i].v : null;
  const res = (items, hint, extra) => Object.assign({ items, from, to, prefix, str, closed, hint: hint || "" }, extra);
  const braceTbl = (br) => br && br.v === "{" ? (prev[br.i - 1] && tb(br.i - 1)) : null;
  // db.X{ ... } context (key or value)
  let dbBr = null, listKey = null;
  if (top && top.v === "{") {
    if (braceTbl(top)) dbBr = top;
    else if (prev[top.i - 1] && prev[top.i - 1].v === "=" && prev[top.i - 2] && prev[top.i - 2].t === "word") { const up = stack[stack.length - 2]; if (up && braceTbl(up)) { dbBr = up; listKey = prev[top.i - 2].v; } }
  }
  if (str) {
    let key = null, kind = null;
    if (pt && pt.v === "[" && pt2 && pt2.v === "counts") kind = "counts"; else if (pt && pt.v === "[" && pt2 && pt2.v === "relations") kind = "rel";
    else if (pt && pt.v === "=" && pt2 && pt2.t === "word" && dbBr) key = pt2.v;
    else if (pt && (pt.v === "," || pt.v === "{") && listKey) key = listKey;
    else if (pt && pt.v === "{" && prev[prev.length - 2] && prev[prev.length - 2].v === "=" && prev[prev.length - 3]) key = prev[prev.length - 3].v;
    else if (top && top.v === "(") { const ch = []; for (let j = top.i - 1; j >= 0 && (prev[j].t === "word" || prev[j].v === "." || prev[j].v === ":"); j--) ch.unshift(prev[j].v); const name = ch.join(""), spec = CALLS[name], idx = prev.slice(top.i + 1).filter((t, j, a) => t.v === "," ).length; const a = Array.isArray(spec) ? spec[idx] : idx === 0 ? spec : null; if (a === "class") kind = "counts"; else if (a === "pred") key = "predicate"; else if (name === "distinct" && idx === 1) return res(["track_id", "class", "source_id", "media_id", "observation_id", "media_time_us"].map((k) => I(k, "col", "row field", `distinct(rows, '${k}')`, { ins: k })), "key to count distinct values of"); }
    if (kind === "counts") return res(DATA.labels.map((l) => I(l, "cls", "class", `Class '${l}'.`, { ins: l })), "class name");
    if (kind === "rel") return res(relKeys().map((r) => I(r.p + ":" + r.l, "val", r.p, `${r.p} relation count for '${r.l}'.`, { ins: r.p + ":" + r.l, bad: r.p === "holding" ? holdBad() : "" })), "predicate:class");
    if (key) { const v = luaValues(key, true, true); return v.length ? res(v, "value for " + key) : null; }
    return null;
  }
  // member access: x.  x:  
  if (pt && (pt.v === "." || pt.v === ":") && pt2 && pt2.t === "word") {
    const ch = luaChain(prev), root = ch[0], items = [];
    if (ch.length === 1 && root === "db") LUA_DB.forEach((r) => items.push(I(r.n, "fn", r.d, r.doc, { ins: r.n + "{${1}}", ex: r.ex, sig: r.d })));
    else if (ch.length === 1 && LIBS[root] && prof !== "x") items.push(...lib(LIBS[root], root));
    else if (ch.length === 1 && root === "ctx" && prof === "rule") items.push(...lib(RULE_CTX, "ctx"));
    else if (ch.length === 1 && sc.vars[root]) items.push(...fieldItems(sc.vars[root]));
    else if (ch.length === 2 && sc.vars[root] === "frames" && ch[1] === "counts") { DATA.labels.forEach((l) => { const id = /^[A-Za-z_]\w*$/.test(l); items.push(I(l, "cls", "number | nil", `Objects of class '${l}' in the frame; nil when unknown.`, { ins: id ? l : `['${l}']`, rf: id ? 0 : 1 })); }); }
    else if (ch.length === 2 && sc.vars[root] === "frames" && ch[1] === "relations") relKeys().forEach((r) => items.push(I(r.p + ":" + r.l, "val", "number | nil", `Count of '${r.p}' relations with '${r.l}'.`, { ins: `['${r.p}:${r.l}']`, rf: 1, bad: r.p === "holding" ? holdBad() : "" })));
    else return null;
    items.forEach((it) => { if (it.rf) it.fromDot = pt.a; });
    return res(items, ch.join(".") + ": members");
  }
  if (dbBr && !listKey) {
    const tn = braceTbl(dbBr);
    if (pt && (pt.v === "{" || pt.v === ",") && top === dbBr) return res(DBKEYS.filter((k) => k.n !== "predicate" || tn === "relations").map((k) => I(k.n, "val", k.d, k.doc, { ins: k.n + (k.d.startsWith("{") ? " = {${1}}" : k.n === "class" || k.d === "string" ? " = '${1}'" : " = "), ex: k.ex, bad: "" })), `db.${tn}{…}: filter keys`);
    if (pt && pt.v === "=" && pt2 && pt2.t === "word") { const v = luaValues(pt2.v, false, false); return v.length ? res(v, "value for " + pt2.v) : null; }
    return null;
  }
  if (listKey) { const v = luaValues(listKey, false, true); return v.length ? res(v, "value for " + listKey) : null; }
  if (!prefix && !o.manual) return null;
  // general identifiers
  const items = [];
  const gl = prof === "rule" ? L(`
count | fn | count("class") | Visible objects of the class in the frame (number). | count("cell phone") >= 4
seen | fn | seen("class") | True when the class is visible in the frame. | seen("knife")
`) : LUA_GLOBALS;
  LUA_KEYWORDS.forEach((k) => items.push(I(k.n, "kw", k.d, k.doc, { ex: k.ex, s: 5 })));
  gl.forEach((g) => { if (prof === "rule" && /^(db|emit|collect|distinct)$/.test(g.n)) return; items.push(I(g.n, g.k === "mod" ? "mod" : g.k === "var" ? "var" : "fn", g.d, g.doc, { ex: g.ex, sig: g.d, s: 2 })); });
  if (prof === "rule") items.push(I("ctx", "var", "typed rule context", "ctx:count(class), ctx:count_people(), ... only in typed Lua rules (rule spec condition.lua).", { bad: "typed Lua rules only (not this one-line editor)", s: 6 }));
  if (prof !== "rule") sc.locals.forEach((n) => items.push(I(n, "var", sc.vars[n] ? "row of " + sc.vars[n] : "local", sc.vars[n] ? `Loop variable bound to a ${sc.vars[n]} row.` : "Local variable.", { s: 1 })));
  else sc.locals.forEach((n) => items.push(I(n, "var", "local", "Local variable.", { s: 1 })));
  LUA_BAD.forEach((b) => { if (prefix.length >= 2 || o.manual) items.push(I(b.n, b.k === "mod" ? "mod" : "fn", "unavailable", "Removed from the sandbox.", { bad: b.bad, s: 9, strict: true })); });
  if (prof !== "rule") items.push(...LUA_SNIPS);
  return res(items, "Lua query: emit(row) adds evidence, return a value");
}
// legacy one-line rule: seen("class") / count("class") >= n / ratio("a","b") >= x joined by and / or, or `lua:` code
function ruleComplete(text, c, o) {
  if (/^\s*lua:/.test(text)) { const off = text.indexOf("lua:") + 4, r = c >= off ? luaComplete(text.slice(off), c - off, Object.assign({}, o, { prof: "rule" })) : null; if (r) { r.from += off; r.to += off; r.items.forEach((it) => { if (it.fromDot != null) it.fromDot += off; }); } return r; }
  const toks = sqlTok(text.replace(/"/g, "'")), strClosed = (v) => /^'(?:[^']|'')*'$/.test(v), tk = toks.find((t) => t.a < c && c <= t.b && (t.t === "word" || t.t === "num" || (t.t === "str" && (c < t.b || !strClosed(t.v)))));
  let from = c, to = c, prefix = "", str = false, closed = false, prev;
  if (tk && tk.t === "str") { str = true; from = tk.a + 1; prefix = text.slice(from, c); closed = strClosed(tk.v); to = closed ? tk.b - 1 : tk.b; prev = toks.filter((t) => t.b <= tk.a); }
  else if (tk && tk.t === "word") { from = tk.a; to = wordEnd(text, c); prefix = text.slice(tk.a, c); prev = toks.filter((t) => t.b <= tk.a); }
  else if (tk) return null; else prev = toks.filter((t) => t.b <= c);
  let st = "term", fnn = "";
  for (const t of prev) {
    const u = t.v.toLowerCase();
    if (st === "term") { if (u === "seen" || u === "count" || u === "ratio") { fnn = u; st = "open"; } else if (t.v === "(") st = "term"; else st = "bad"; }
    else if (st === "open") st = t.v === "(" ? "arg1" : "bad";
    else if (st === "arg1") st = t.t === "str" ? "after1" : "bad";
    else if (st === "after1") st = t.v === "," && fnn === "ratio" ? "arg2" : t.v === ")" ? (fnn === "seen" ? "done" : "op") : "bad";
    else if (st === "arg2") st = t.t === "str" ? "after2" : "bad";
    else if (st === "after2") st = t.v === ")" ? "op" : "bad";
    else if (st === "op") st = t.t === "op" || t.v === "=" ? "num" : "bad";
    else if (st === "num") st = t.t === "num" ? "done" : "bad";
    else if (st === "done") st = u === "and" || u === "or" ? "term" : t.v === ")" ? "done" : "bad";
    else st = "bad";
  }
  const out = [], mk = (r) => ({ items: out, from, to, prefix, str, closed, hint: r });
  if (str) { if (st === "arg1" || st === "arg2") { DATA.labels.forEach((l) => out.push(I(l, "cls", "class", `Class '${l}'.`, { ins: l }))); return mk("class name"); } return null; }
  if (st === "term") { out.push(I('seen("class")', "snip", "template", "True when the class is visible in the frame.", { fr: "seen", ins: 'seen("${1:person}")', ex: 'seen("knife")' }), I('count("class") >= n', "snip", "template", "Visible objects of the class compared with a number (integer).", { fr: "count", ins: 'count("${1:person}") ${2:>=} ${3:1}', ex: 'count("cell phone") >= 4' }), I('ratio("a", "b") >= x', "snip", "template", "Ratio of two class counts compared with a decimal. Unknown when the denominator is 0.", { fr: "ratio", ins: 'ratio("${1:cell phone}", "${2:person}") ${3:>=} ${4:0.2}', ex: 'ratio("cell phone", "person") >= 0.2' })); if (!text.trim()) out.push(I("lua:", "kw", "Lua rule", "Prefix for a Lua rule: uses count(\"class\") and seen(\"class\") as functions.", { ex: 'lua: count("person") > 3 and seen("knife")', ins: "lua: " })); return mk("rule: seen(), count() or ratio()"); }
  if (st === "op") { RULE_OPS.forEach(([o2, d]) => out.push(I(o2, "op", d, `Comparison: ${d}.`, { ins: o2 + " " }))); return mk("comparison"); }
  if (st === "num") { ["1", "2", "5", "10", "0.2", "0.5"].forEach((v) => out.push(I(v, "val", "number", "count() takes an integer, ratio() a decimal.", { ins: v }))); return mk("number"); }
  if (st === "done") { out.push(I("and", "kw", "both", "Both conditions must hold.", { ins: "and " }), I("or", "kw", "either", "Either condition may hold.", { ins: "or " })); return mk("and / or"); }
  return null;
}

/* ---- ranking ---- */
function fuzzy(q, s) {
  if (!q) return { sc: 0, idx: [] };
  const ql = q.toLowerCase(), sl = s.toLowerCase(), r = (a, n) => Array.from({ length: n }, (_, i) => a + i);
  if (sl.startsWith(ql)) return { sc: 1000 - s.length, idx: r(0, q.length) };
  const at = sl.indexOf(ql);
  if (at > 0 && /[^a-z0-9]/.test(sl[at - 1])) return { sc: 700 - at, idx: r(at, q.length) };
  const idx = []; let j = 0, gaps = 0, p = -2;
  for (let i = 0; i < sl.length && j < ql.length; i++) if (sl[i] === ql[j]) { idx.push(i); if (i !== p + 1) gaps++; p = i; j++; }
  return j < ql.length ? null : { sc: 300 - gaps * 10 - idx[0], idx };
}
const parseSnip = (t) => {
  const re = /\$(\d)|\$\{(\d):([^}]*)\}|\$\{(\d)\}/g; let out = "", last = 0, m; const stops = [];
  while ((m = re.exec(t))) { out += t.slice(last, m.index); last = re.lastIndex; const n = Number(m[1] ?? m[2] ?? m[4]), d = m[3] || ""; stops.push({ n, a: out.length, b: out.length + d.length }); out += d; }
  out += t.slice(last);
  stops.sort((x, y) => (x.n || 99) - (y.n || 99));
  return { text: out, stops };
};
const KIND_TITLE = { kw: "keyword", tbl: "table", col: "column", fn: "function", snip: "snippet", cls: "class", src: "source", val: "value", var: "variable", op: "operator", mod: "module" };
let EDN = 0;
const BR = { "(": ")", "[": "]", "{": "}" }, BRC = { ")": "(", "]": "[", "}": "{" };

function makeEditor(box, onRun, opts) {
  opts = opts || {};
  const ta = box.querySelector("textarea"), hl = box.querySelector(".ed-hl"), er = box.querySelector(".ed-err"), gut = box.querySelector(".ed-gut > div"), lang = box.dataset.lang, body = box.querySelector(".ed-body");
  const kind = opts.kind || (lang === "sql" ? "sql" : "lua"), uid = ++EDN, wrap = box.parentNode;
  const provide = kind === "sql" ? sqlComplete : kind === "rule" ? ruleComplete : (t, c, o) => luaComplete(t, c, Object.assign({ prof: "q" }, o));
  let err = null;
  const ed = {
    ta, setError(r) { err = r; paint(); },
    get() { return ta.value; },
    set(v) { ta.value = v; err = null; session = null; closePop(); paint(); }
  };
  // ----- bracket-match overlay, status line, live region
  const br = document.createElement("pre"); br.className = "ed-br"; br.setAttribute("aria-hidden", "true"); body.insertBefore(br, hl);
  const probe = document.createElement("span"); probe.className = "ed-probe"; probe.setAttribute("aria-hidden", "true"); probe.textContent = "0".repeat(50); body.appendChild(probe);
  const stEl = document.createElement("div"); stEl.className = "ed-status"; wrap.appendChild(stEl);
  const live = document.createElement("div"); live.className = "sr-only"; live.setAttribute("aria-live", "polite"); wrap.appendChild(live);
  ta.setAttribute("role", "combobox"); ta.setAttribute("aria-autocomplete", "list"); ta.setAttribute("aria-haspopup", "listbox"); ta.setAttribute("aria-expanded", "false");
  const LN = { sql: "SQL", lua: "Lua", rule: "Rule" }[kind];

  function paint() {
    const v = ta.value, n = v.split("\n").length;
    hl.innerHTML = highlight(v, lang) + "\n";
    if (err) { const a = Math.max(0, Math.min(err.start, v.length)), b = Math.max(a + 1, Math.min(err.end, v.length)); er.innerHTML = esc(v.slice(0, a)) + '<span class="e">' + esc(v.slice(a, b) || " ") + "</span>" + esc(v.slice(b)) + "\n"; } else er.innerHTML = "";
    const eline = err ? v.slice(0, err.start).split("\n").length : 0;
    let g = ""; for (let i = 1; i <= n; i++) g += i === eline ? `<b class="bad">${i}</b>\n` : i + "\n";
    gut.innerHTML = g; box.style.setProperty("--gw", String(String(n).length + 1.4) + "ch");
    sync();
  }
  function sync() { hl.scrollTop = er.scrollTop = br.scrollTop = ta.scrollTop; hl.scrollLeft = er.scrollLeft = br.scrollLeft = ta.scrollLeft; gut.style.transform = `translateY(${-ta.scrollTop}px)`; }
  let inputs = 0;
  function ins(text) { const n = inputs; if (!document.execCommand || !document.execCommand("insertText", false, text)) { ta.setRangeText(text, ta.selectionStart, ta.selectionEnd, "end"); } if (n === inputs) ta.dispatchEvent(new Event("input", { bubbles: true })); }

  // ----- caret geometry (monospace; tab-size 2)
  let M = null;
  const metrics = () => {
    if (M && M.ok) return M; const cs = getComputedStyle(ta), fs = parseFloat(cs.fontSize) || 13;
    const pw = probe.getBoundingClientRect().width / 50;
    return (M = { cw: pw || fs * 0.6, ok: !!pw, lh: parseFloat(cs.lineHeight) || fs * 1.5, pl: parseFloat(cs.paddingLeft) || 0, pt: parseFloat(cs.paddingTop) || 0 });
  };
  addEventListener("resize", () => { M = null; place(); });
  if (document.fonts && document.fonts.ready) document.fonts.ready.then(() => { M = null; });
  function colOf(v, i) { const ls = v.lastIndexOf("\n", i - 1) + 1; let col = 0; for (let k = ls; k < i; k++) col += v[k] === "\t" ? 2 - (col % 2) : 1; return col; }
  function caretXY(i) {
    const m = metrics(), v = ta.value, line = v.slice(0, i).split("\n").length - 1, r = ta.getBoundingClientRect();
    return { x: r.left + m.pl + colOf(v, i) * m.cw - ta.scrollLeft, y: r.top + m.pt + line * m.lh - ta.scrollTop, lh: m.lh, box: r };
  }
  function indexAt(cx, cy) {
    const m = metrics(), r = ta.getBoundingClientRect(), v = ta.value, lines = v.split("\n");
    const li = Math.floor((cy - r.top - m.pt + ta.scrollTop) / m.lh), col = Math.floor((cx - r.left - m.pl + ta.scrollLeft) / m.cw);
    if (li < 0 || li >= lines.length || col < 0 || col >= lines[li].length) return -1;
    let off = 0; for (let k = 0; k < li; k++) off += lines[k].length + 1; return off + col;
  }

  // ----- popup
  let pop = null, list = null, docEl = null, items = [], sel = 0, res = null, timer = 0, quiet = false, session = null, prevVal = ta.value, lastDedent = -1;
  const isOpen = () => !!pop && !pop.hidden;
  function ensurePop() {
    if (pop) return;
    pop = document.createElement("div"); pop.className = "ed-pop"; pop.hidden = true;
    pop.innerHTML = `<div class="ed-list" role="listbox" id="edl${uid}" aria-label="Suggestions"></div><div class="ed-doc"></div>`;
    list = pop.firstChild; docEl = pop.lastChild; document.body.appendChild(pop);
    pop.addEventListener("mousedown", (e) => e.preventDefault());
    list.addEventListener("click", (e) => { const it = e.target.closest(".ed-it"); if (it) accept(Number(it.dataset.i)); });
    list.addEventListener("mousemove", (e) => { const it = e.target.closest(".ed-it"); if (it && Number(it.dataset.i) !== sel) select(Number(it.dataset.i), true); });
  }
  function closePop() {
    clearTimeout(timer); if (!pop || pop.hidden) { ta.setAttribute("aria-expanded", "false"); return; }
    pop.hidden = true; items = []; res = null; ta.setAttribute("aria-expanded", "false"); ta.removeAttribute("aria-activedescendant"); live.textContent = "";
  }
  const sched = (manual) => { clearTimeout(timer); timer = setTimeout(() => compute(!!manual), 20); };
  function compute(manual) {
    if (document.activeElement !== ta || ta.selectionStart !== ta.selectionEnd) return closePop();
    loadData().then(() => {}, () => {});
    let r = null; try { r = provide(ta.value, ta.selectionStart, { manual }); } catch (e) { r = null; }
    if (!r) return closePop();
    const q = r.prefix, scored = [];
    r.items.forEach((it, n) => { const m = fuzzy(q, it.fr || it.l); if (!m || (it.strict && m.sc < 1000)) return; scored.push({ it, sc: m.sc - (it.bad ? 40 : 0) - (it.s || 0) * 0.5 - n * 0.001, idx: it.fr && it.fr !== it.l ? [] : m.idx }); });
    scored.sort((a, b) => b.sc - a.sc);
    const top = scored.slice(0, 50);
    if (!top.length || (top.length === 1 && !manual && top[0].it.l === q && !top[0].it.ins.includes("$"))) return closePop();
    ensurePop(); res = r; items = top.map((s) => Object.assign({}, s.it, { idx: s.idx })); sel = 0;
    list.innerHTML = items.map((it, i) => `<div class="ed-it${it.bad ? " bad" : ""}" role="option" id="edo${uid}-${i}" data-i="${i}" aria-selected="${i === 0}"><span class="ed-k k-${it.k}" title="${KIND_TITLE[it.k] || it.k}">${it.k}</span><span class="ed-l">${hlLabel(it)}</span><span class="ed-d">${esc(it.bad || it.d)}</span></div>`).join("");
    pop.hidden = false; ta.setAttribute("aria-expanded", "true"); ta.setAttribute("aria-controls", "edl" + uid);
    live.textContent = items.length + (items.length === 1 ? " suggestion" : " suggestions") + (items.some((i) => i.bad) ? ", some unsupported" : "");
    select(0); place(); status();
  }
  const hlLabel = (it) => { const set = new Set(it.idx || []); return [...it.l].map((ch, i) => (set.has(i) ? `<b>${esc(ch)}</b>` : esc(ch))).join(""); };
  function docHtml(it) {
    if (!it) return "";
    return `<div class="ed-dh"><span class="ed-k k-${it.k}">${it.k}</span> <code>${esc(it.sig || it.l)}</code></div>${it.d && !it.sig ? `<div class="ed-dd">${esc(it.d)}</div>` : ""}${it.doc ? `<p>${esc(it.doc)}</p>` : ""}${it.ex ? `<pre>${esc(it.ex)}</pre>` : ""}${it.bad ? `<p class="ed-bad">unsupported: ${esc(it.bad)}</p>` : ""}`;
  }
  function select(i, noScroll) {
    if (!items.length) return; sel = (i + items.length) % items.length;
    list.querySelectorAll(".ed-it[aria-selected=true]").forEach((n) => n.setAttribute("aria-selected", "false"));
    const el = list.children[sel]; el.setAttribute("aria-selected", "true"); ta.setAttribute("aria-activedescendant", el.id);
    if (!noScroll) el.scrollIntoView({ block: "nearest" });
    const it = items[sel]; docEl.innerHTML = docHtml(it); docEl.hidden = !(it.doc || it.ex || it.bad || it.sig);
  }
  function place() {
    if (!isOpen() || !res) return;
    const vw = document.documentElement.clientWidth, vh = innerHeight, p = caretXY(Math.max(res.from, 0)), wide = vw >= 760;
    pop.classList.toggle("stack", !wide);
    const w = wide ? Math.min(560, vw - 16) : Math.min(360, vw - 16); pop.style.width = w + "px";
    let left = Math.min(p.x, vw - w - 8); if (p.box.width >= w) left = Math.max(p.box.left, Math.min(left, p.box.right - w)); left = Math.max(8, left);
    pop.style.maxHeight = ""; pop.style.left = left + "px";
    let h = pop.offsetHeight, below = vh - (p.y + p.lh) - 10, above = p.y - 10;
    const goAbove = h > below && above > below; const room = goAbove ? above : below;
    pop.style.maxHeight = Math.max(120, room) + "px"; h = Math.min(h, Math.max(120, room));
    pop.style.top = (goAbove ? p.y - h - 2 : p.y + p.lh + 2) + "px"; pop.classList.toggle("above", goAbove);
    if (goAbove) hideSig();
  }
  addEventListener("scroll", () => { place(); placeSig(); hideHover(); }, true);
  ta.addEventListener("scroll", () => { place(); placeSig(); });
  ta.addEventListener("blur", () => setTimeout(() => { if (document.activeElement !== ta) { closePop(); hideSig(); hideHover(); } }, 120));
  ta.addEventListener("focus", () => { loadData().then(() => {}, () => {}); status(); });
  ta.addEventListener("click", () => { closePop(); caretMoved(); });

  // ----- accepting an item (replaces the current word, keeps the caret right, supports tab stops)
  function accept(i) {
    const it = items[i], r = res; if (!it || !r) return;
    const a = it.fromDot != null ? it.fromDot : r.from, b = r.to, v = ta.value, ls = v.lastIndexOf("\n", a - 1) + 1, indent = (v.slice(ls, a).match(/^[ \t]*/) || [""])[0];
    const quote = r.str ? v[r.from - 1] : "", p = parseSnip(it.ins.replace(/\n/g, "\n" + indent));
    const text = p.text + (r.str && !r.closed ? quote : ""), again = !it.bad && (/ $/.test(p.text) || /\{\$\{?\d\}?\}$/.test(it.ins));
    quiet = true; ta.setSelectionRange(a, b); ins(text); quiet = false;
    prevVal = ta.value; closePop();
    if (p.stops.length) {
      const st = p.stops.filter((s) => s.n !== 0).map((s) => ({ a: a + s.a, b: a + s.b })), fin = p.stops.find((s) => s.n === 0);
      if (st.length) { session = { st, i: 0, end: fin ? a + fin.a : a + text.length }; ta.setSelectionRange(st[0].a, st[0].b); }
      else ta.setSelectionRange(fin ? a + fin.a : a + text.length, fin ? a + fin.a : a + text.length);
    } else { const e = a + text.length + (r.str && r.closed ? 1 : 0); ta.setSelectionRange(e, e); }
    if (again) sched(false);
    status(); sigSoon();
  }
  function stop(dir) {
    if (!session) return false;
    const n = session.i + dir;
    if (n >= session.st.length) { const e = session.end; session = null; ta.setSelectionRange(e, e); return true; }
    if (n < 0) return true;
    session.i = n; ta.setSelectionRange(session.st[n].a, session.st[n].b); return true;
  }
  function shiftStops(old, cur) {
    if (!session) return; let p = 0; const max = Math.min(old.length, cur.length);
    while (p < max && old[p] === cur[p]) p++;
    let s = 0; while (s < max - p && old[old.length - 1 - s] === cur[cur.length - 1 - s]) s++;
    const oe = old.length - s, d = cur.length - old.length, ca = session.st[session.i];
    if (!ca || p > ca.b + 0 || p < ca.a) { session = null; return; }
    session.st.forEach((t, k) => { if (k === session.i) t.b += d; else if (t.a >= oe) { t.a += d; t.b += d; } });
    if (session.end >= oe) session.end += d;
  }

  // ----- literals / brackets
  function inLit(v, c) {
    const toks = kind === "sql" ? sqlTok(v.slice(0, c)) : kind === "rule" ? sqlTok(v.slice(0, c).replace(/"/g, "'")) : luaTok(v.slice(0, c)), l = toks[toks.length - 1];
    if (!l || l.b !== c) return false;
    if (kind === "lua") return (l.t === "str" || l.t === "com") && unterminated(l);
    return l.t === "str" && !/^'(?:[^']|'')*'$/.test(l.v);
  }
  const QUOTES = kind === "sql" ? "'" : kind === "rule" ? '"' : "'\"", OPENS = kind === "lua" ? "([{" : "(";
  function autoPair(e) {
    const k = e.key, v = ta.value, s = ta.selectionStart, nx = v[s], pv = v[s - 1];
    if (ta.selectionStart !== ta.selectionEnd) return false;
    if ((")]}'\"".includes(k)) && nx === k && (k in BRC ? (OPENS.includes(BRC[k])) : QUOTES.includes(k))) { e.preventDefault(); closePop(); ta.setSelectionRange(s + 1, s + 1); return true; }
    const okNext = nx === undefined || /[\s)\]};,]/.test(nx);
    if (OPENS.includes(k) && k in BR && okNext) { e.preventDefault(); ins(k + BR[k]); ta.setSelectionRange(s + 1, s + 1); if (k === "{") sched(false); return true; }
    if (QUOTES.includes(k) && okNext && !/\w/.test(pv || "") && !inLit(v, s)) { e.preventDefault(); ins(k + k); ta.setSelectionRange(s + 1, s + 1); sched(false); return true; }
    return false;
  }
  function toggleComment() {
    const v = ta.value, s = ta.selectionStart, e = ta.selectionEnd, ls = v.lastIndexOf("\n", s - 1) + 1; let le = v.indexOf("\n", e > s && v[e - 1] === "\n" ? e - 1 : e); if (le < 0) le = v.length;
    const lines = v.slice(ls, le).split("\n"), all = lines.filter((l) => l.trim()).every((l) => /^\s*--/.test(l));
    const min = Math.min(...lines.filter((l) => l.trim()).map((l) => l.match(/^\s*/)[0].length), 1e9);
    const out = lines.map((l) => !l.trim() ? l : all ? l.replace(/^(\s*)-- ?/, "$1") : l.slice(0, min) + "-- " + l.slice(min)).join("\n");
    ta.setSelectionRange(ls, le); ins(out); ta.setSelectionRange(ls, ls + out.length);
  }
  let brKey = "";
  function matchBracket() {
    const v = ta.value, s = ta.selectionStart; let key = "", i = -1;
    if (ta.selectionEnd === s && v.length < 30000) { for (const p of [s - 1, s]) { const ch = v[p]; if (ch && (ch in BR || ch in BRC) && OPENS.concat(")]}").includes(ch)) { i = p; break; } } }
    if (i < 0) { if (brKey) { br.innerHTML = ""; brKey = ""; } return; }
    const ch = v[i], open = ch in BR, toks = kind === "sql" ? sqlTok(v) : kind === "rule" ? sqlTok(v.replace(/"/g, "'")) : luaTok(v), skip = toks.filter((t) => t.t === "str" || t.t === "com");
    const lit = (p) => skip.some((t) => p >= t.a && p < t.b); if (lit(i)) { br.innerHTML = ""; brKey = ""; return; }
    let d = 0, j = -1; const step = open ? 1 : -1, tgt = open ? BR[ch] : BRC[ch];
    for (let p = i; p >= 0 && p < v.length; p += step) { const c = v[p]; if (lit(p)) continue; if (c === ch) d++; else if (c === tgt && --d === 0) { j = p; break; } }
    key = i + ":" + j + ":" + v.length; if (key === brKey) return; brKey = key;
    if (j < 0) { br.innerHTML = esc(v.slice(0, i)) + '<span class="m bad">' + esc(ch) + "</span>" + esc(v.slice(i + 1)) + "\n"; return; }
    const a = Math.min(i, j), b = Math.max(i, j);
    br.innerHTML = esc(v.slice(0, a)) + '<span class="m">' + esc(v[a]) + "</span>" + esc(v.slice(a + 1, b)) + '<span class="m">' + esc(v[b]) + "</span>" + esc(v.slice(b + 1)) + "\n";
  }

  // ----- status line
  let stRaf = 0;
  function status() {
    if (stRaf) return; stRaf = requestAnimationFrame(() => {
      stRaf = 0; const v = ta.value, c = ta.selectionStart, ln = v.slice(0, c).split("\n").length, col = c - v.lastIndexOf("\n", c - 1);
      let hint = ""; if (isOpen() && res) hint = res.hint; else { try { const r = provide(v, c, { manual: true }); hint = (r && r.hint) || ""; } catch (e) { hint = ""; } }
      stEl.innerHTML = `<span class="ed-lang">${LN}</span><span>Ln ${ln}, Col ${col}</span><span class="ed-hint">${esc(hint)}</span><span class="ed-keys">Ctrl+Space suggestions</span>`;
      try { matchBracket(); } catch (e) { /* overlay is cosmetic */ }
    });
  }

  // ----- signature help
  let sig = null;
  function sigAt(v, c) {
    if (kind === "sql" || kind === "rule" && !/^\s*lua:/.test(v)) {
      const t = sqlTok(kind === "rule" ? v.replace(/"/g, "'") : v).filter((x) => x.b <= c), st = []; let lastStr = false;
      t.forEach((x, i) => { if (x.v === "(") st.push({ i, c: 0 }); else if (x.v === ")") st.pop(); else if (x.v === "," && st.length) st[st.length - 1].c++; });
      void lastStr; const o = st[st.length - 1]; if (!o) return null; const cal = t[o.i - 1]; if (!cal) return null; const u = cal.v.toUpperCase();
      if (kind === "rule") { const n = u.toLowerCase(), s = n === "seen" ? ['seen("class")', 0] : n === "count" ? ['count("class") >= n', 0] : n === "ratio" ? ['ratio("numerator", "denominator") >= x', o.c] : null; return s && { text: s[0], act: s[1], doc: n === "seen" ? "True when the class is visible in the frame." : n === "count" ? "Visible objects of the class, compared with an integer." : "Count ratio compared with a decimal; unknown when the denominator is 0." }; }
      if (u === "COUNT" && t[o.i - 2] && t[o.i - 2].v.toUpperCase() === "SELECT" || u === "COUNT" && !/\bFROM\b/i.test(v.slice(0, cal.a))) return { text: "COUNT(*)  |  COUNT(DISTINCT column)", act: 0, doc: "Rows, or unique values of one column." };
      if (u === "COUNT") return { text: "count('class')", act: 0, doc: "Frames table: objects of that class in the frame." };
      if (u === "IN" || (u === "NOT" && false)) return { text: "column IN ('a', 'b', …)", act: o.c, doc: "Quoted strings or numbers, matching the column type." };
      return null;
    }
    const src = kind === "rule" ? v.slice(v.indexOf("lua:") + 4) : v, cc = kind === "rule" ? c - v.indexOf("lua:") - 4 : c;
    if (cc < 0) return null;
    const toks = luaTok(src.slice(0, cc)), last = toks[toks.length - 1];
    if (last && last.b === cc && last.t === "com" && unterminated(last)) return null;
    const st = []; toks.forEach((t, i) => { if (t.t !== "sym") return; if ("([{".includes(t.v)) st.push({ v: t.v, i, c: 0 }); else if (")]}".includes(t.v)) st.pop(); else if (t.v === "," && st.length) st[st.length - 1].c++; });
    for (let k = st.length - 1; k >= 0; k--) {
      const o = st[k]; let name = "", j = o.i - 1; while (j >= 0 && (toks[j].t === "word" || toks[j].v === "." || toks[j].v === ":")) { name = toks[j].v + name; j--; }
      const s = SIGS[name]; if (!s || (o.v === "{") !== !!s.brace || o.v === "[") continue;
      if (s.brace) { let key = null, d = 0; for (let m = o.i + 1; m < toks.length; m++) { const t = toks[m]; if ("([{".includes(t.v) && t.t === "sym") d++; else if (")]}".includes(t.v) && t.t === "sym") d--; else if (d === 0 && t.v === ",") key = null; else if (d === 0 && t.t === "word" && toks[m + 1] && toks[m + 1].v === "=" && key === null) key = t.v; }
        const typed = last && last.t === "word" && last.b === cc && toks[toks.length - 2] && /[{,]/.test(toks[toks.length - 2].v) ? last.v : null; if (typed && !key) key = s.args.find((a) => a.startsWith(typed)) || null;
        return { text: name + "{ " + s.args.join(", ") + " }", act: key, args: s.args, doc: s.doc }; }
      return { text: name + "(" + s.args.join(", ") + ")" + (s.ret ? " -> " + s.ret : ""), act: o.c, args: s.args, doc: s.doc };
    }
    return null;
  }
  function renderSig(s) {
    let h;
    if (s.args && Array.isArray(s.args)) { const act = typeof s.act === "string" ? s.args.indexOf(s.act) : s.act, br2 = s.text.indexOf("{") >= 0; h = esc(s.text.slice(0, s.text.search(br2 ? /\{/ : /\(/) + 1)) + (br2 ? " " : "") + s.args.map((a, i) => i === act ? `<b>${esc(a)}</b>` : esc(a)).join(", ") + esc(s.text.slice(s.text.indexOf(br2 ? " }" : ")"))); }
    else h = esc(s.text);
    return `<code>${h}</code>${s.doc ? `<div class="ed-dd">${esc(s.doc)}</div>` : ""}`;
  }
  function ensureSig() { if (sig) return; sig = document.createElement("div"); sig.className = "ed-sig"; sig.setAttribute("role", "tooltip"); sig.hidden = true; document.body.appendChild(sig); }
  function hideSig() { if (sig) sig.hidden = true; }
  let sigState = null, sigT = 0;
  const sigSoon = () => { clearTimeout(sigT); sigT = setTimeout(() => { if (document.activeElement !== ta) return; let s = null; try { s = ta.selectionStart === ta.selectionEnd ? sigAt(ta.value, ta.selectionStart) : null; } catch (e) { s = null; } sigState = s; if (!s) return hideSig(); ensureSig(); sig.innerHTML = renderSig(s); sig.hidden = false; placeSig(); }, 30); };
  function placeSig() {
    if (!sig || sig.hidden) return; if (isOpen() && pop.classList.contains("above")) { sig.hidden = true; return; }
    const vw = document.documentElement.clientWidth, p = caretXY(ta.selectionStart), w = Math.min(sig.offsetWidth, vw - 16), h = sig.offsetHeight;
    sig.style.left = Math.max(8, Math.min(p.x, vw - w - 8)) + "px"; sig.style.top = (p.y - h - 4 < 6 ? p.y + p.lh + 4 : p.y - h - 4) + "px";
    if (isOpen() && sig.style.top && parseFloat(sig.style.top) > p.y) sig.hidden = true;
  }

  // ----- hover docs
  let hov = null, hovT = 0, hovAt = -2;
  const hideHover = () => { clearTimeout(hovT); hovAt = -2; if (hov) hov.hidden = true; };
  function hoverDoc(v, i) {
    const m = /\w*/.exec(v.slice(i)), pre = /\w*$/.exec(v.slice(0, i)); if (!m || !pre) return null; const word = pre[0] + m[0]; if (!word || /^\d/.test(word)) return null;
    const a = i - pre[0].length;
    if (kind === "sql" || (kind === "rule" && !/^\s*lua:/.test(v))) {
      const u = word.toUpperCase(), lw = word.toLowerCase();
      if (SQL_DOC[u] && kind === "sql") { const k = SQL_DOC[u]; return I(u, "kw", k[0], k[1], { ex: k[2] }); }
      if (kind === "rule") { const s = SIGS[lw]; return s && (lw === "seen" || lw === "count" || lw === "ratio") ? I(lw, "fn", "", lw === "seen" ? "True when the class is visible in the frame." : lw === "count" ? "Visible objects of the class, compared with an integer." : "Count ratio compared with a decimal.", { sig: renderSigPlain(lw) }) : (lw === "and" || lw === "or") ? I(lw, "kw", "", lw === "and" ? "Both conditions must hold." : "Either condition may hold.") : null; }
      if (SQL_TABLES[lw]) { const t = SQL_TABLES[lw]; return I(lw, "tbl", t[0], t[1], { ex: t[2], bad: t[3] || "" }); }
      const toks = sqlTok(v), ft = toks.findIndex((t) => t.t === "word" && t.v.toUpperCase() === "FROM"), tn = ft >= 0 && toks[ft + 1] ? toks[ft + 1].v.toLowerCase() : "detections", tb = tn === "sightings" || !SQL_TABLES[tn] ? "detections" : tn;
      const col = sqlCols(tb).find((x) => x.n === lw); if (col) return I(col.n, "col", (col.t === T_ ? "text" : "number") + " · " + col.d, col.doc);
      return null;
    }
    const off = kind === "rule" ? v.indexOf("lua:") + 4 : 0, src = v.slice(off), ii = i - off, aa = a - off; if (ii < 0) return null;
    const chain = []; let p = aa - 1; while (p >= 1 && (src[p] === "." || src[p] === ":") && /\w/.test(src[p - 1])) { const w = /\w+$/.exec(src.slice(0, p)); chain.unshift(w[0]); p -= w[0].length + 1; }
    const full = chain.concat(word), nm = full.join("."), nxt = /^\s*=(?!=)/.test(src.slice(ii + m[0].length));
    const asDoc = (r, k, pfx) => I(r.n, k || (r.k === "var" ? "var" : "fn"), r.d, r.doc, { sig: r.d, ex: r.ex, bad: r.bad });
    if (full.length === 2 && full[0] === "db") { const r = LUA_DB.find((x) => x.n === word); if (r) return asDoc(r); }
    if (full.length === 2 && LIBS[full[0]]) { const r = LIBS[full[0]].find((x) => x.n === word); if (r) return asDoc(r); }
    if (full.length === 1) {
      if (nxt) { const k = DBKEYS.find((x) => x.n === word); if (k) return I(k.n, "val", k.d, k.doc, { ex: k.ex }); }
      const g = LUA_KEYWORDS.concat(LUA_GLOBALS).find((x) => x.n === word); if (g) return asDoc(g, g.k === "kw" ? "kw" : null);
      const b = LUA_BAD.find((x) => x.n === word); if (b) return I(b.n, "fn", "unavailable", "Removed from the sandbox.", { bad: b.bad });
      const sc = luaScope(luaTok(src.slice(0, ii))); if (sc.vars[word]) return I(word, "var", "row of " + sc.vars[word], `Loop variable bound to a ${sc.vars[word]} row.`);
    }
    if (full.length === 2) { const sc = luaScope(luaTok(src.slice(0, ii))), tb = sc.vars[full[0]]; if (tb) { const f = (ROWF[tb] || []).find((x) => x[0] === word); if (f) return I(word, "col", f[1], ROWDOC[word] || `${word} of a ${tb} row.`); } if (full[0] === "ctx") { const r = RULE_CTX.find((x) => x.n === word); if (r) return asDoc(r); } }
    void nm; return null;
  }
  const renderSigPlain = (n) => (n === "seen" ? 'seen("class")' : n === "count" ? 'count("class") >= n' : 'ratio("a", "b") >= x');
  ta.addEventListener("mousemove", (e) => {
    const i = indexAt(e.clientX, e.clientY);
    if (i === hovAt) return; clearTimeout(hovT); if (hov && !hov.hidden) hov.hidden = true; hovAt = i; if (i < 0) return;
    hovT = setTimeout(() => {
      let it = null; try { it = hoverDoc(ta.value, i); } catch (er2) { it = null; } if (!it || isOpen()) return;
      if (!hov) { hov = document.createElement("div"); hov.className = "ed-hover"; hov.setAttribute("role", "tooltip"); document.body.appendChild(hov); }
      hov.innerHTML = docHtml(it); hov.hidden = false; const vw = document.documentElement.clientWidth, w = Math.min(hov.offsetWidth, vw - 16), h = hov.offsetHeight;
      hov.style.left = Math.max(8, Math.min(e.clientX, vw - w - 8)) + "px"; hov.style.top = (e.clientY + 18 + h > innerHeight ? Math.max(6, e.clientY - h - 8) : e.clientY + 18) + "px";
    }, 500);
  });
  ta.addEventListener("mouseleave", hideHover);

  // ----- events
  function caretMoved() { status(); sigSoon(); }
  ta.addEventListener("keyup", (e) => { if (/^(Arrow|Home|End|Page)/.test(e.key) && !(isOpen() && /Up|Down|Page/.test(e.key))) caretMoved(); });
  document.addEventListener("selectionchange", () => { if (document.activeElement === ta) status(); });
  ta.addEventListener("input", (e) => {
    inputs++; err = null; const old = prevVal; paint(); prevVal = ta.value; box.dispatchEvent(new Event("ed-input"));
    if (quiet) return;
    if (session) shiftStops(old, ta.value);
    const t = e.inputType || "", ch = e.data;
    if (kind === "lua" && t === "insertText" && ch && /^[a-z]$/.test(ch)) dedent();
    if (!/^(insert|delete)/.test(t)) closePop();
    else if (t.startsWith("insert") && ch && ch.length === 1) {
      const trig = /\w/.test(ch) || ch === "'" || ch === '"' || (kind !== "sql" && ".:{".includes(ch)) || ch === ",";
      if (trig) sched(false); else closePop();
    } else if (t.startsWith("delete")) { lastDedent = -1; if (isOpen()) sched(false); else closePop(); }
    else closePop();
    status(); sigSoon();
  });
  function dedent() {
    const v = ta.value, c = ta.selectionStart, ls = v.lastIndexOf("\n", c - 1) + 1, m = /^([ \t]+)(end|else|elseif|until)$/.exec(v.slice(ls, c));
    if (!m || /\w/.test(v[c] || "") || lastDedent === ls && m[2] !== "end" && m[2] !== "until") return;
    const cut = m[1].replace(/\t$| {1,2}$/, ""); if (cut === m[1]) return; lastDedent = ls;
    quiet = true; ta.setSelectionRange(ls, ls + m[1].length); ins(cut); quiet = false; prevVal = ta.value;
  }
  ta.addEventListener("scroll", sync);
  ta.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); closePop(); onRun(); return; }
    const mod = e.ctrlKey || e.metaKey, plain = !mod && !e.altKey;
    if (e.ctrlKey && !e.metaKey && (e.key === " " || e.code === "Space")) { e.preventDefault(); ensurePop(); compute(true); return; }
    if (isOpen() && res && (ta.selectionStart < res.from || ta.selectionStart > res.to || ta.value.slice(res.from, ta.selectionStart) !== res.prefix)) closePop(); // caret left the word
    if (isOpen()) {
      if (e.key === "ArrowDown") { e.preventDefault(); select(sel + 1); return; }
      if (e.key === "ArrowUp") { e.preventDefault(); select(sel - 1); return; }
      if (e.key === "PageDown") { e.preventDefault(); select(Math.min(items.length - 1, sel + 8)); return; }
      if (e.key === "PageUp") { e.preventDefault(); select(Math.max(0, sel - 8)); return; }
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closePop(); return; }
      if ((e.key === "Tab" && !e.shiftKey || e.key === "Enter") && plain) {
        // The word is already complete: Enter/Tab keep their normal meaning (newline / indent) instead of a no-op accept.
        if (res && items[sel] && items[sel].ins === ta.value.slice(res.from, ta.selectionStart) && !res.str) closePop();
        else { e.preventDefault(); accept(sel); return; }
      }
      if (/^(Arrow(Left|Right)|Home|End|Enter|Tab)$/.test(e.key)) closePop();
    }
    if (e.key === "Escape") { if (session) { session = null; e.stopPropagation(); } hideSig(); hideHover(); return; }
    if (session && e.key === "Tab" && plain) { e.preventDefault(); stop(e.shiftKey ? -1 : 1); return; }
    if (mod && e.key === "/" && kind === "lua") { e.preventDefault(); toggleComment(); return; }
    if (plain && e.key.length === 1 && autoPair(e)) return;
    if (e.key === "Backspace" && plain && ta.selectionStart === ta.selectionEnd) {
      const v = ta.value, s = ta.selectionStart, a = v[s - 1], b = v[s];
      if (a && b && ((a in BR && BR[a] === b && OPENS.includes(a)) || (QUOTES.includes(a) && a === b))) { e.preventDefault(); ta.setSelectionRange(s - 1, s + 1); document.execCommand("delete"); return; }
    }
    if (e.key === "Tab") {
      e.preventDefault();
      const v = ta.value, s = ta.selectionStart, en = ta.selectionEnd;
      if (s === en && !e.shiftKey) { ins("  "); return; }
      const ls = v.lastIndexOf("\n", s - 1) + 1, block = v.slice(ls, en), lines = block.split("\n");
      const nb = lines.map((l) => e.shiftKey ? l.replace(/^ {1,2}/, "") : "  " + l).join("\n");
      ta.setSelectionRange(ls, en); ins(nb); ta.setSelectionRange(ls, ls + nb.length);
    } else if (e.key === "Enter" && !e.shiftKey) {
      const v = ta.value, s = ta.selectionStart, ls = v.lastIndexOf("\n", s - 1) + 1, ind = (v.slice(ls, s).match(/^[ \t]*/) || [""])[0];
      const extra = lang === "lua" && /(\bthen|\bdo|\bfunction\b[^\n]*\)|\belse|\brepeat)\s*$/.test(v.slice(ls, s)) ? "  " : "";
      e.preventDefault(); ins("\n" + ind + extra);
    }
  });
  ed.hoverDoc = hoverDoc; ed.sigAt = sigAt;
  paint(); status(); return ed;
}
  return { highlight, makeEditor, data: DATA, loadData, fuzzy, sqlComplete, luaComplete, ruleComplete };
})();
