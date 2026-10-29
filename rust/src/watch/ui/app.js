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
    if (!r.ok) throw new Error(j.error || "Could not save");
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
    $("f-ops").textContent = s.figures.ops;
    $("f-lack").textContent = s.figures.lacking;
    $("f-pcs").textContent = s.figures.pcs;
    $("f-warn").textContent = s.figures.warnings;
  }
  function start(paint, ms) {
    async function tick() {
      try {
        const r = await fetch("/api/state");
        if (!r.ok) return;
        const s = await r.json();
        Watch.state = s;
        meters(s);
        if (paint) paint(s);
      } catch (_) {}
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
