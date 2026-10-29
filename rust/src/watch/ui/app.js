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
  function create() {
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
      const red = Watch.reduced(), used = new Set();
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
        s.conf = d.confidence; s.cls = d.class; s.hue = hueOf(d.class, d.track_id);
        s.trail.push([(bb[0] + bb[2]) / 2, bb[3]]); if (s.trail.length > TRAIL) s.trail.shift();
        used.add(s);
      }
      for (const s of states.values()) if (!used.has(s) && !s.lostAt) { s.from = s.to = pos(s, now); s.dur = 0; s.lostAt = red ? -1e9 : now; }
      const n = [...used].filter((s) => s.cls === "person").length;
      if (n !== count) { count = n; countT = now; }
    };
    an.paint = (g, r, now, o) => {
      o = Object.assign({ boxes: true, labels: true, conf: true, trail: false, thresh: 0.5, badge: true }, o);
      let busy = false;
      g.save(); g.lineCap = "round"; g.lineJoin = "round"; g.font = "11px " + (getComputedStyle(document.documentElement).getPropertyValue("--font-mono") || "monospace"); g.textBaseline = "middle";
      for (const [key, s] of states) {
        const inK = Math.min(1, (now - s.born) / FADE_IN), outK = s.lostAt ? 1 - (now - s.lostAt) / FADE_OUT : 1;
        if (outK <= 0) { states.delete(key); continue; }
        const [x0, y0, x1, y1] = pos(s, now);
        if (inK < 1 || s.lostAt || (s.dur && now - s.t0 < s.dur)) busy = true;
        const a = ease(inK) * (s.lostAt ? ease(outK) : 1), sc = 0.94 + 0.06 * ease(inK);
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
function makeEditor(box, onRun) {
  const ta = box.querySelector("textarea"), hl = box.querySelector(".ed-hl"), er = box.querySelector(".ed-err"), gut = box.querySelector(".ed-gut > div"), lang = box.dataset.lang;
  let err = null;
  const ed = {
    ta, setError(r) { err = r; paint(); },
    get() { return ta.value; },
    set(v) { ta.value = v; err = null; paint(); }
  };
  function paint() {
    const v = ta.value, n = v.split("\n").length;
    hl.innerHTML = highlight(v, lang) + "\n";
    if (err) { const a = Math.max(0, Math.min(err.start, v.length)), b = Math.max(a + 1, Math.min(err.end, v.length)); er.innerHTML = esc(v.slice(0, a)) + '<span class="e">' + esc(v.slice(a, b) || " ") + "</span>" + esc(v.slice(b)) + "\n"; } else er.innerHTML = "";
    const eline = err ? v.slice(0, err.start).split("\n").length : 0;
    let g = ""; for (let i = 1; i <= n; i++) g += i === eline ? `<b class="bad">${i}</b>\n` : i + "\n";
    gut.innerHTML = g; box.style.setProperty("--gw", String(String(n).length + 1.4) + "ch");
    sync();
  }
  function sync() { hl.scrollTop = er.scrollTop = ta.scrollTop; hl.scrollLeft = er.scrollLeft = ta.scrollLeft; gut.style.transform = `translateY(${-ta.scrollTop}px)`; }
  function ins(text) { if (!document.execCommand || !document.execCommand("insertText", false, text)) ta.setRangeText(text, ta.selectionStart, ta.selectionEnd, "end"); }
  ta.addEventListener("input", () => { err = null; paint(); box.dispatchEvent(new Event("ed-input")); });
  ta.addEventListener("scroll", sync);
  ta.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); onRun(); return; }
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
  paint(); return ed;
}
  return { highlight, makeEditor };
})();
