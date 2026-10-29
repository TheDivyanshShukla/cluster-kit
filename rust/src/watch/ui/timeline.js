/* Watch review timeline: canvas, windowed. Only visible lanes and visible matches are drawn.
   Lane clocks: each lane has its own origin (recording start). Lanes never imply simultaneity unless
   the caller sets lane.off (microseconds) for a known shared clock. */
(() => {
  const LH = 66, RULER = 22, GUT = 150, STRIP_T = 16, STRIP_H = 40, TILE_W = 71, STILL_W = 60, GAP_US = 1e6;
  const NICE = [0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 14400, 21600, 43200, 86400];

  function fmt(us, ms) {
    const neg = us < 0; us = Math.abs(us);
    const s = Math.floor(us / 1e6), h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, ss = s % 60;
    const p = (n) => String(n).padStart(2, "0");
    let out = (h ? h + ":" + p(m) : m) + ":" + p(ss);
    if (ms) out += "." + String(Math.floor((us % 1e6) / 1000)).padStart(3, "0");
    return (neg ? "-" : "") + out;
  }
  function bisect(arr, key, v) { let lo = 0, hi = arr.length; while (lo < hi) { const m = (lo + hi) >> 1; if (key(arr[m]) < v) lo = m + 1; else hi = m; } return lo; }
  let rvEl = null;
  const cssv = (n) => getComputedStyle(rvEl || (rvEl = document.querySelector('.review')) || document.documentElement).getPropertyValue(n).trim();

  class Timeline {
    constructor(host, cb) {
      this.host = host; this.cb = cb; host.classList.add("tl");
      host.innerHTML = '<div class="tl-sizer"></div><canvas class="tl-cv" tabindex="0" role="application" aria-label="Timeline. Click a strip to review it. J and K, or the bracket keys, move between matches. Plus and minus zoom."></canvas>';
      this.cv = host.querySelector("canvas"); this.sizer = host.firstChild; this.ctx = this.cv.getContext("2d");
      this.lanes = []; this.mode = "orig"; this.pps = 40; this.cur = null; this.curLane = 0; this.t = 0; this.hasT = false;
      this.edit = {}; this.items = {}; this.follow = true; this.dirty = true; this.drag = null; this.hover = null;
      this.vis = []; // visible hit boxes this frame
      this.col = {};
      host.addEventListener("scroll", () => this.redraw(), { passive: true });
      new ResizeObserver(() => this.resize()).observe(host);
      this.cv.addEventListener("pointerdown", (e) => this.down(e));
      this.cv.addEventListener("pointermove", (e) => this.move(e));
      this.cv.addEventListener("pointerup", (e) => this.up(e));
      this.cv.addEventListener("pointerleave", () => { if (!this.drag) this.setHover(null, 0, 0); });
      this.cv.addEventListener("wheel", (e) => {
        if (e.ctrlKey || e.metaKey) { e.preventDefault(); this.zoomBy(e.deltaY < 0 ? 1.25 : 0.8, e.clientX - this.cv.getBoundingClientRect().left); }
        else if (e.shiftKey && !e.deltaX) { e.preventDefault(); host.scrollLeft += e.deltaY; }
      }, { passive: false });
      this.resize(); this.loop();
    }
    loop() { if (this.dirty) { this.dirty = false; const t0 = performance.now(); this.drawn = 0; this.draw(); this.ms = performance.now() - t0; } requestAnimationFrame(() => this.loop()); }
    redraw() { this.dirty = true; }
    resize() {
      const w = this.host.clientWidth, h = this.host.clientHeight, d = window.devicePixelRatio || 1;
      if (!w || !h) return;
      this.W = w; this.H = h; this.cv.width = Math.round(w * d); this.cv.height = Math.round(h * d);
      this.cv.style.width = w + "px"; this.cv.style.height = h + "px"; this.d = d; this.layout(); this.redraw();
    }
    colors() {
      for (const [k, v] of [["lead", "--lead"], ["sup", "--support"], ["pop", "--pop"], ["page", "--page"], ["surf", "--surface"], ["line", "--line"], ["quiet", "--quiet"], ["text", "--text"], ["bad", "--bad"]]) this.col[k] = cssv(v) || "#888";
    }
    // ---- data
    setLanes(lanes) {
      this.lanes = lanes;
      for (const l of lanes) {
        l.matches.sort((a, b) => a.context_start_us - b.context_start_us || a.start_us - b.start_us);
        let pk = 0, mx = 0;
        for (const m of l.matches) { m.pk = pk; const len = m.context_end_us - m.context_start_us; pk += len + GAP_US; if (len > mx) mx = len; }
        l.packedLen = pk; l.maxLen = mx;
        if (l.still) l.matches.forEach((m, n) => { m.si = n; });
      }
      this.layout(); this.redraw();
    }
    contentUs() {
      let mx = 1e6;
      for (const l of this.lanes) if (!l.still) mx = Math.max(mx, this.mode === "orig" ? (l.dur || 0) + (l.off || 0) : l.packedLen);
      return mx;
    }
    layout() {
      this.colors();
      let w = GUT + Math.ceil(this.contentUs() / 1e6 * this.pps) + 60;
      for (const l of this.lanes) if (l.still) w = Math.max(w, GUT + l.matches.length * STILL_W + 60);
      this.sizer.style.width = w + "px"; this.sizer.style.height = RULER + this.lanes.length * LH + "px";
    }
    setMode(m) { const was = this.pos(); this.mode = m; this.layout(); this.fitAll(); if (was) this.redraw(); this.cb.onMode && this.cb.onMode(m); }
    setEdits(e) { this.edit = e; this.redraw(); }
    setItems(i) { this.items = i; this.redraw(); }
    setCurrent(m, laneIdx) { this.cur = m; if (laneIdx != null) this.curLane = laneIdx; this.redraw(); }
    setPlayhead(laneIdx, t, noFollow) {
      this.curLane = laneIdx; this.t = t; this.hasT = true;
      if (this.follow && !noFollow && !this.drag) this.reveal();
      this.redraw();
    }
    // ---- geometry. k = px per microsecond
    get k() { return this.pps / 1e6; }
    ecs(m) { const e = this.edit[m.id]; return e ? e.cs : m.context_start_us; }
    ece(m) { const e = this.edit[m.id]; return e ? e.ce : m.context_end_us; }
    // x in canvas coords of media time t within match m (matched view) or lane (orig view)
    xOf(lane, t, m) {
      const sl = this.host.scrollLeft;
      if (this.mode === "orig") return GUT + (t + (lane.off || 0)) * this.k - sl;
      return GUT + ((m ? m.pk + (t - m.context_start_us) : 0)) * this.k - sl;
    }
    tAt(lane, x, m) { // inverse
      const sl = this.host.scrollLeft;
      if (this.mode === "orig") return (x - GUT + sl) / this.k - (lane.off || 0);
      const u = (x - GUT + sl) / this.k;
      if (!m) return 0;
      return m.context_start_us + (u - m.pk);
    }
    matchAtX(lane, x) { // matched view: block containing x; orig view: not used
      const u = (x - GUT + this.host.scrollLeft) / this.k;
      const i = bisect(lane.matches, (m) => m.pk, u + 0.5) - 1;
      const m = lane.matches[i];
      return m && u <= m.pk + (m.context_end_us - m.context_start_us) ? m : null;
    }
    pos() { const l = this.lanes[this.curLane]; return l; }
    visible(lane, x0, x1) { // matches whose block intersects [x0,x1] canvas px
      const sl = this.host.scrollLeft, out = [], arr = lane.matches;
      const u0 = (x0 - GUT + sl) / this.k, u1 = (x1 - GUT + sl) / this.k;
      if (this.mode === "orig") {
        const off = lane.off || 0, pad = 60e6;
        let i = bisect(arr, (m) => m.context_start_us, u0 - off - lane.maxLen - pad);
        for (; i < arr.length; i++) { const m = arr[i]; if (m.context_start_us + off - pad > u1) break; if (this.ece(m) + off >= u0 && this.ecs(m) + off <= u1) out.push(m); }
      } else {
        let i = bisect(arr, (m) => m.pk, u0 - lane.maxLen - GAP_US);
        for (; i < arr.length; i++) { const m = arr[i]; if (m.pk > u1) break; if (m.pk + (m.context_end_us - m.context_start_us) >= u0) out.push(m); }
      }
      return out;
    }
    reveal() {
      const l = this.lanes[this.curLane]; if (!l) return;
      const m = this.mode === "matched" ? (this.cur && this.cur.media_id === l.media_id ? this.cur : null) : null;
      if (this.mode === "matched" && !m) return;
      const x = this.xOf(l, this.t, m);
      if (x < GUT + 20 || x > this.W - 40) this.host.scrollLeft = Math.max(0, this.host.scrollLeft + x - (GUT + (this.W - GUT) * 0.3));
    }
    scrollToMatch(m, laneIdx) {
      const l = this.lanes[laneIdx]; if (!l) return;
      const x = l.still ? GUT + m.si * STILL_W - this.host.scrollLeft : this.xOf(l, this.ecs(m), m);
      if (x < GUT || x > this.W - 80) this.host.scrollLeft = Math.max(0, this.host.scrollLeft + x - GUT - 40);
      const y = RULER + laneIdx * LH, st = this.host.scrollTop;
      if (y < st || y + LH > st + this.H) this.host.scrollTop = Math.max(0, y - this.H / 2 + LH / 2);
    }
    // ---- zoom
    setPps(pps, ax) {
      const min = Math.max(0.0005, (this.W - GUT - 80) / (this.contentUs() / 1e6)), max = 5000;
      pps = Math.max(min, Math.min(max, pps));
      if (ax == null) ax = GUT + (this.W - GUT) / 2;
      const sec = (this.host.scrollLeft + ax - GUT) / this.pps;
      this.pps = pps; this.layout();
      this.host.scrollLeft = Math.max(0, sec * pps - (ax - GUT)); this.redraw();
      this.cb.onZoom && this.cb.onZoom(pps);
    }
    zoomBy(f, ax) { this.setPps(this.pps * f, ax); }
    fitAll() { this.pps = Math.max(0.0005, (this.W - GUT - 80) / (this.contentUs() / 1e6)); this.layout(); this.host.scrollLeft = 0; this.redraw(); this.cb.onZoom && this.cb.onZoom(this.pps); }
    fitRange(laneIdx, m, a, b) { // a,b in media us
      const l = this.lanes[laneIdx]; if (!l || b <= a) return;
      const pad = (b - a) * 0.15; a -= pad; b += pad;
      this.pps = Math.min(5000, Math.max(0.0005, (this.W - GUT - 60) / ((b - a) / 1e6))); this.layout();
      const base = this.mode === "orig" ? a + (l.off || 0) : m.pk + (a - m.context_start_us);
      this.host.scrollLeft = Math.max(0, base * this.k); this.redraw(); this.cb.onZoom && this.cb.onZoom(this.pps);
    }
    // ---- drawing
    draw() {
      const c = this.ctx, d = this.d || 1, col = this.col, W = this.W, H = this.H; if (!W) return;
      c.setTransform(d, 0, 0, d, 0, 0); c.clearRect(0, 0, W, H);
      c.fillStyle = col.page; c.fillRect(0, 0, W, H);
      const st = this.host.scrollTop, sl = this.host.scrollLeft;
      this.vis = [];
      const i0 = Math.max(0, Math.floor((st - RULER) / LH)), i1 = Math.min(this.lanes.length - 1, Math.floor((st + H - RULER) / LH));
      c.font = '11px "IBM Plex Mono", monospace'; c.textBaseline = "middle";
      for (let i = i0; i <= i1; i++) this.drawLane(c, i, RULER + i * LH - st);
      this.drawRuler(c);
      // gutter
      c.fillStyle = col.surf; c.fillRect(0, RULER, GUT, H);
      for (let i = i0; i <= i1; i++) this.drawGutter(c, this.lanes[i], i, RULER + i * LH - st);
      c.fillStyle = col.surf; c.fillRect(0, 0, GUT, RULER);
      c.fillStyle = col.quiet; c.fillText(this.mode === "orig" ? "lane-relative time" : "matched only", 10, RULER / 2);
      c.strokeStyle = col.line; c.beginPath(); c.moveTo(GUT + .5, 0); c.lineTo(GUT + .5, H); c.stroke();
      if (!this.lanes.length) { c.fillStyle = col.quiet; c.fillText("No lanes.", GUT + 16, RULER + 24); }
    }
    drawRuler(c) {
      const col = this.col, W = this.W;
      c.fillStyle = col.page; c.fillRect(GUT, 0, W - GUT, RULER);
      c.strokeStyle = col.line; c.beginPath(); c.moveTo(0, RULER - .5); c.lineTo(W, RULER - .5); c.stroke();
      if (this.mode === "orig") {
        const secPerPx = 1 / this.pps, want = 80 * secPerPx; let step = NICE.find((n) => n >= want) || 86400;
        const s0 = Math.floor((this.host.scrollLeft) * secPerPx / step) * step, s1 = (this.host.scrollLeft + W - GUT) * secPerPx;
        c.fillStyle = col.quiet; c.strokeStyle = col.line; c.textAlign = "left";
        for (let s = s0; s <= s1; s += step) {
          const x = GUT + s * this.pps - this.host.scrollLeft; if (x < GUT) continue;
          c.beginPath(); c.moveTo(x + .5, RULER - 8); c.lineTo(x + .5, RULER); c.stroke();
          c.fillText(fmt(s * 1e6), x + 4, RULER / 2 - 2);
        }
      } else { c.fillStyle = col.quiet; c.fillText("gaps between matches removed - time is not continuous", GUT + 12, RULER / 2); }
      // playhead marker
      const l = this.lanes[this.curLane];
      if (l && this.hasT) {
        const m = this.mode === "matched" ? (this.cur && this.cur.media_id === l.media_id ? this.cur : null) : null;
        if (this.mode === "orig" || m) {
          const x = this.xOf(l, this.t, m);
          if (x >= GUT && x <= W) { c.fillStyle = col.pop; c.beginPath(); c.moveTo(x - 5, RULER - 9); c.lineTo(x + 5, RULER - 9); c.lineTo(x, RULER - 1); c.fill(); }
        }
      }
    }
    drawGutter(c, l, i, y) {
      const col = this.col, cur = i === this.curLane;
      c.fillStyle = cur ? "#1c1c1c" : col.surf; c.fillRect(0, y, GUT, LH);
      c.strokeStyle = col.line; c.beginPath(); c.moveTo(0, y + LH - .5); c.lineTo(GUT, y + LH - .5); c.stroke();
      if (cur) { c.fillStyle = col.pop; c.fillRect(0, y, 3, LH); }
      c.textAlign = "left"; c.fillStyle = col.text; c.font = '600 12px "IBM Plex Sans", sans-serif';
      c.fillText(this.cut(c, l.label, GUT - 20), 12, y + 14);
      c.font = '11px "IBM Plex Mono", monospace'; c.fillStyle = col.quiet;
      c.fillText(this.cut(c, l.sub || "", GUT - 20), 12, y + 30);
      const tag = l.tag;
      if (tag) { c.fillStyle = tag.warn ? col.sup : col.quiet; const lines = this.wrap(c, tag.text, GUT - 20); lines.slice(0, 2).forEach((t, n) => c.fillText(t, 12, y + 46 + n * 12)); }
    }
    cut(c, s, w) { if (c.measureText(s).width <= w) return s; while (s.length > 1 && c.measureText(s + "…").width > w) s = s.slice(0, -1); return s + "…"; }
    wrap(c, s, w) { const out = []; let cur = ""; for (const wd of s.split(" ")) { if (cur && c.measureText(cur + " " + wd).width > w) { out.push(cur); cur = wd; } else cur = cur ? cur + " " + wd : wd; } if (cur) out.push(cur); return out; }
    drawLane(c, i, y) {
      const l = this.lanes[i], col = this.col, W = this.W;
      c.save(); c.beginPath(); c.rect(GUT, Math.max(RULER, y), W - GUT, LH); c.clip();
      c.fillStyle = i % 2 ? "#0e0e0e" : col.page; c.fillRect(GUT, y, W - GUT, LH);
      if (l.clock === "media") { c.fillStyle = "rgba(206,125,54,.06)"; c.fillRect(GUT, y, W - GUT, LH); }
      c.strokeStyle = col.line; c.beginPath(); c.moveTo(GUT, y + LH - .5); c.lineTo(W, y + LH - .5); c.stroke();
      const sl = this.host.scrollLeft, bad = l.availability === "missing" || l.availability === "expired" || l.recorded === false;
      // recording extent
      if (this.mode === "orig" && l.dur) {
        const x0 = GUT + (l.off || 0) * this.k - sl, x1 = x0 + l.dur * this.k;
        c.fillStyle = "rgba(197,207,203,.04)"; c.fillRect(x0, y + 4, x1 - x0, LH - 8);
        c.strokeStyle = "rgba(197,207,203,.25)"; c.strokeRect(x0 + .5, y + 4.5, x1 - x0 - 1, LH - 9);
        // local tick labels for media-relative lanes
        if (l.clock === "media") {
          const want = 90 / this.pps, step = NICE.find((n) => n >= want) || 86400; c.fillStyle = col.quiet; c.textAlign = "left";
          for (let s = Math.max(0, Math.floor((sl) / this.pps / step) * step); s * 1e6 <= l.dur && GUT + s * this.pps - sl < W; s += step) {
            const x = GUT + s * this.pps - sl + (l.off || 0) * this.k; if (x < GUT) continue; c.fillText(fmt(s * 1e6), x + 3, y + 11);
          }
        }
        // indexing gaps
        for (const g of l.gaps || []) {
          const gx0 = GUT + (g.start_us + (l.off || 0)) * this.k - sl, gx1 = GUT + (g.end_us + (l.off || 0)) * this.k - sl;
          if (gx1 < GUT || gx0 > W) continue;
          c.fillStyle = "rgba(206,125,54,.28)"; c.fillRect(gx0, y + LH - 12, Math.max(2, gx1 - gx0), 7);
          c.fillStyle = "rgba(10,10,10,.9)"; for (let hx = Math.max(gx0, GUT); hx < Math.min(gx1, W); hx += 6) c.fillRect(hx, y + LH - 12, 1, 7);
          this.vis.push({ gap: g, lane: i, x0: gx0, x1: Math.max(gx1, gx0 + 3), y0: y + LH - 12, y1: y + LH - 5 });
        }
      }
      if (l.recorded === false) { c.fillStyle = col.sup; c.textAlign = "left"; c.fillText("Historical footage was not recorded for this live source.", GUT + 14, y + LH / 2); }
      else if (l.availability === "missing" || l.availability === "expired") {
        c.fillStyle = "rgba(181,72,63,.10)"; c.fillRect(GUT, y, W - GUT, LH);
      }
      if (l.still) { this.drawStill(c, l, i, y); c.restore(); return; }
      const arr = this.visible(l, GUT, W), cur = this.cur, still = l.still;
      let drawn = 0;
      for (const m of arr) {
        const cs = this.ecs(m), ce = this.ece(m);
        const xs = this.xOf(l, m.start_us, m), xe = this.xOf(l, m.end_us, m), xcs = this.xOf(l, cs, m), xce = this.xOf(l, ce, m);
        const sy = y + STRIP_T, isCur = cur && cur.id === m.id, it = this.items[m.id];
        const w = Math.max(still ? 14 : 2, xe - xs), x = still ? xs : xs;
        // context
        if (xce - xcs > w + 1) {
          c.fillStyle = "rgba(206,125,54,.16)"; c.fillRect(xcs, sy + 4, xce - xcs, STRIP_H - 8);
          if (xce - xcs > 6) { c.setLineDash([3, 3]); c.strokeStyle = "rgba(206,125,54,.7)"; c.strokeRect(xcs + .5, sy + 4.5, xce - xcs - 1, STRIP_H - 9); c.setLineDash([]); }
        }
        // matched interval
        const conf = .55 + .45 * (m.max_confidence || 0);
        c.globalAlpha = bad ? .45 : conf;
        c.fillStyle = m.verification === "rejected" ? col.bad : col.lead; c.fillRect(x, sy, w, STRIP_H);
        c.globalAlpha = 1;
        if (w >= TILE_W * .7 && !bad && this.cb.tile) {
          const n = Math.max(1, Math.round(w / TILE_W)), tw = w / n;
          c.save(); c.beginPath(); c.rect(x, sy, w, STRIP_H); c.clip();
          for (let t = 0; t < n; t++) {
            const tm = m.start_us + (t + .5) / n * (m.end_us - m.start_us);
            const tile = this.cb.tile(m, tm, n);
            if (tile) c.drawImage(tile.img, tile.sx, tile.sy, tile.sw, tile.sh, x + t * tw, sy, tw, STRIP_H);
            else { c.fillStyle = "rgba(0,0,0,.35)"; c.fillRect(x + t * tw + 1, sy + 1, tw - 2, STRIP_H - 2); }
            if (t) { c.fillStyle = "rgba(10,10,10,.8)"; c.fillRect(x + t * tw, sy, 1, STRIP_H); }
          }
          c.restore();
        }
        if (w > 44) { c.fillStyle = "rgba(10,10,10,.75)"; c.fillRect(x, sy + STRIP_H - 13, Math.min(w, 66), 13); c.fillStyle = col.pop; c.textAlign = "left"; c.fillText(fmt(m.start_us), x + 3, sy + STRIP_H - 6); }
        if (m.verification !== "unreviewed") { c.fillStyle = m.verification === "confirmed" ? "#6fd0c0" : m.verification === "rejected" ? "#e07a70" : col.pop; c.fillRect(x, sy + STRIP_H, w, 3); }
        c.strokeStyle = isCur ? col.pop : "rgba(10,10,10,.85)"; c.lineWidth = isCur ? 2 : 1; c.strokeRect(x + .5, sy + .5, w - 1, STRIP_H - 1); c.lineWidth = 1;
        if (it) { // in selection: orange tab + trim
          c.fillStyle = col.sup; c.fillRect(x, sy - 4, Math.min(w, 14), 3);
          if (isCur) {
            const xi = this.xOf(l, it.in_us, m), xo = this.xOf(l, it.out_us, m);
            c.fillStyle = "rgba(10,10,10,.55)"; c.fillRect(Math.min(xcs, xi), sy, Math.max(0, xi - xcs), STRIP_H); c.fillRect(xo, sy, Math.max(0, xce - xo), STRIP_H);
            c.fillStyle = col.sup; c.fillRect(xi - 1, sy - 3, 3, STRIP_H + 6); c.fillRect(xo - 1, sy - 3, 3, STRIP_H + 6);
            c.fillRect(xi - 1, sy - 3, 8, 3); c.fillRect(xo - 7, sy - 3, 8, 3);
            this.vis.push({ handle: "in", m, lane: i, x0: xi - 5, x1: xi + 5, y0: sy - 4, y1: sy + STRIP_H + 4 }, { handle: "out", m, lane: i, x0: xo - 5, x1: xo + 5, y0: sy - 4, y1: sy + STRIP_H + 4 });
          }
        }
        if (isCur) { // context handles
          c.fillStyle = col.quiet; c.fillRect(xcs - 1, sy + 8, 3, STRIP_H - 16); c.fillRect(xce - 1, sy + 8, 3, STRIP_H - 16);
          this.vis.push({ handle: "cs", m, lane: i, x0: xcs - 5, x1: xcs + 5, y0: sy + 4, y1: sy + STRIP_H - 4 }, { handle: "ce", m, lane: i, x0: xce - 5, x1: xce + 5, y0: sy + 4, y1: sy + STRIP_H - 4 });
        }
        this.vis.push({ m, lane: i, x0: x, x1: x + w, y0: sy, y1: sy + STRIP_H });
        drawn++;
      }
      this.drawn += drawn;
      // playhead in own lane
      if (i === this.curLane && this.hasT) {
        const m = this.mode === "matched" ? (cur && cur.media_id === l.media_id ? cur : null) : null;
        if (this.mode === "orig" || m) { const x = this.xOf(l, this.t, m); if (x >= GUT && x <= W) { c.fillStyle = col.pop; c.fillRect(x - .5, y, 1.5, LH); } }
      }
      c.restore();
    }
    drawStill(c, l, i, y) {
      const col = this.col, sl = this.host.scrollLeft, a = Math.max(0, Math.floor(sl / STILL_W) - 1), b = Math.min(l.matches.length - 1, Math.ceil((sl + this.W) / STILL_W));
      c.textAlign = "left"; let n = 0;
      for (let j = a; j <= b; j++) {
        const m = l.matches[j], x = GUT + j * STILL_W - sl + 4, sy = y + STRIP_T, isCur = this.cur && this.cur.id === m.id, w = STILL_W - 8;
        c.fillStyle = m.verification === "rejected" ? col.bad : col.lead; c.globalAlpha = .5 + .5 * (m.max_confidence || 0); c.fillRect(x, sy, w, STRIP_H); c.globalAlpha = 1;
        const tile = this.cb.still && this.cb.still(m);
        if (tile) { try { c.drawImage(tile, x, sy, w, STRIP_H); } catch (_) {} }
        c.fillStyle = "rgba(10,10,10,.75)"; c.fillRect(x, sy + STRIP_H - 12, w, 12); c.fillStyle = col.pop; c.fillText("#" + (j + 1), x + 3, sy + STRIP_H - 5);
        if (m.verification !== "unreviewed") { c.fillStyle = m.verification === "confirmed" ? "#6fd0c0" : m.verification === "rejected" ? "#e07a70" : col.pop; c.fillRect(x, sy + STRIP_H, w, 3); }
        c.strokeStyle = isCur ? col.pop : "rgba(10,10,10,.85)"; c.lineWidth = isCur ? 2 : 1; c.strokeRect(x + .5, sy + .5, w - 1, STRIP_H - 1); c.lineWidth = 1;
        if (this.items[m.id]) { c.fillStyle = col.sup; c.fillRect(x, sy - 4, 14, 3); }
        this.vis.push({ m, lane: i, x0: x, x1: x + w, y0: sy, y1: sy + STRIP_H }); n++;
      }
      this.drawn += n;
    }
    // ---- pointer
    local(e) { const r = this.cv.getBoundingClientRect(); return { x: e.clientX - r.left, y: e.clientY - r.top }; }
    hit(x, y) {
      for (let n = this.vis.length - 1; n >= 0; n--) { const v = this.vis[n]; if (x >= v.x0 && x <= v.x1 && y >= v.y0 && y <= v.y1 && !v.gap) { if (v.handle || x > GUT) return v; } }
      for (let n = this.vis.length - 1; n >= 0; n--) { const v = this.vis[n]; if (v.gap && x >= v.x0 && x <= v.x1 && y >= v.y0 && y <= v.y1) return v; }
      return null;
    }
    laneAtY(y) { const i = Math.floor((y - RULER + this.host.scrollTop) / LH); return i >= 0 && i < this.lanes.length ? i : -1; }
    down(e) {
      this.cv.focus({ preventScroll: true });
      const { x, y } = this.local(e);
      if (y < RULER && x > GUT) { this.drag = { type: "ruler" }; this.cv.setPointerCapture(e.pointerId); this.scrub(x); return; }
      const li = this.laneAtY(y);
      if (x <= GUT) { if (li >= 0) this.cb.onLane && this.cb.onLane(li); return; }
      const h = this.hit(x, y);
      if (h && h.handle) { this.drag = { type: "handle", h }; this.cv.setPointerCapture(e.pointerId); return; }
      if (h && h.m) {
        const l = this.lanes[h.lane];
        const t = l.still ? h.m.start_us : this.tAt(l, x, h.m);
        this.cb.onPick(h.m, h.lane, this.cur && this.cur.id === h.m.id ? Math.max(h.m.start_us - 1e6, Math.min(h.m.end_us + 1e6, t)) : null);
        return;
      }
      if (li >= 0) { this.drag = { type: "lane", li }; this.cv.setPointerCapture(e.pointerId); this.scrub(x, li); }
    }
    scrub(x, li) {
      if (li == null) li = this.curLane;
      const l = this.lanes[li]; if (!l || l.still || l.recorded === false) return;
      let m = null;
      if (this.mode === "matched") { m = this.matchAtX(l, x); if (!m) return; }
      let t = this.tAt(l, x, m);
      if (l.dur) t = Math.max(0, Math.min(l.dur, t));
      this.cb.onSeek(li, t, m);
    }
    move(e) {
      const { x, y } = this.local(e), d = this.drag;
      if (d) {
        if (d.type === "ruler" || d.type === "lane") this.scrub(x, d.type === "lane" ? d.li : null);
        else if (d.type === "handle") { const l = this.lanes[d.h.lane]; this.cb.onHandle(d.h.handle, d.h.m, this.tAt(l, x, d.h.m), false); }
        if (x > this.W - 30) this.host.scrollLeft += 12; else if (x < GUT + 30) this.host.scrollLeft -= 12;
        return;
      }
      const h = x > GUT ? this.hit(x, y) : null;
      this.cv.style.cursor = h && h.handle ? "ew-resize" : h ? "pointer" : y < RULER ? "col-resize" : "default";
      this.setHover(h, e.clientX, e.clientY);
    }
    up(e) {
      const d = this.drag; this.drag = null;
      if (d && d.type === "handle") { const { x } = this.local(e); this.cb.onHandle(d.h.handle, d.h.m, this.tAt(this.lanes[d.h.lane], x, d.h.m), true); }
    }
    setHover(h, cx, cy) { const k = h ? (h.m ? h.m.id : "g") : null; if (k === this.hk && !h) return; this.hk = k; this.cb.onHover && this.cb.onHover(h, cx, cy); }
  }

  // ---- Rail: horizontally windowed row of fixed blocks (matched-only sequence, selected clips)
  class Rail {
    constructor(host, o) {
      this.host = host; this.o = o; host.classList.add("rail");
      host.innerHTML = '<div class="tl-sizer"></div><canvas class="tl-cv" tabindex="0" role="listbox"></canvas>';
      this.cv = host.querySelector("canvas"); this.cv.setAttribute("aria-label", o.label); this.sizer = host.firstChild; this.ctx = this.cv.getContext("2d");
      this.items = []; this.xs = [0]; this.focus = 0; this.sel = null; this.dirty = true; this.dragI = -1; this.dropI = -1; this.col = {};
      host.addEventListener("scroll", () => this.redraw(), { passive: true });
      new ResizeObserver(() => this.resize()).observe(host);
      this.cv.addEventListener("pointerdown", (e) => this.down(e)); this.cv.addEventListener("pointermove", (e) => this.move(e)); this.cv.addEventListener("pointerup", (e) => this.up(e));
      this.cv.addEventListener("keydown", (e) => this.key(e));
      this.cv.addEventListener("focus", () => this.redraw()); this.cv.addEventListener("blur", () => this.redraw());
      this.resize(); const lp = () => { if (this.dirty) { this.dirty = false; this.draw(); } requestAnimationFrame(lp); }; lp();
    }
    redraw() { this.dirty = true; }
    resize() { const w = this.host.clientWidth, h = this.host.clientHeight, d = window.devicePixelRatio || 1; if (!w || !h) return; this.W = w; this.H = h; this.d = d; this.cv.width = w * d; this.cv.height = h * d; this.cv.style.width = w + "px"; this.cv.style.height = h + "px"; this.redraw(); }
    set(items, sel) { this.items = items; this.sel = sel; this.xs = [0]; for (const it of items) this.xs.push(this.xs[this.xs.length - 1] + this.o.w(it) + 4); this.sizer.style.width = this.xs[this.xs.length - 1] + 8 + "px"; this.sizer.style.height = "1px"; this.focus = Math.min(this.focus, Math.max(0, items.length - 1)); this.redraw(); }
    indexAt(x) { const u = x + this.host.scrollLeft; const i = bisect(this.xs, (v) => v, u + .01) - 1; return i >= 0 && i < this.items.length && u < this.xs[i + 1] - 4 ? i : -1; }
    reveal(i) { const a = this.xs[i], b = this.xs[i + 1]; if (a == null) return; if (a < this.host.scrollLeft) this.host.scrollLeft = a - 8; else if (b > this.host.scrollLeft + this.W) this.host.scrollLeft = b - this.W + 8; }
    down(e) { const r = this.cv.getBoundingClientRect(), i = this.indexAt(e.clientX - r.left); if (i < 0) return; this.focus = i; this.o.pick(this.items[i], i); if (this.o.reorder) { this.dragI = i; this.dropI = i; this.sx = e.clientX; this.moved = false; this.cv.setPointerCapture(e.pointerId); } this.redraw(); }
    move(e) {
      if (this.dragI < 0) return; if (Math.abs(e.clientX - this.sx) > 6) this.moved = true; if (!this.moved) return;
      const r = this.cv.getBoundingClientRect(); const u = e.clientX - r.left + this.host.scrollLeft;
      let j = bisect(this.xs, (v) => v, u) - 1; this.dropI = Math.max(0, Math.min(this.items.length - 1, j)); this.redraw();
    }
    up() { if (this.dragI >= 0 && this.moved && this.dropI !== this.dragI) this.o.reorder(this.dragI, this.dropI); this.dragI = -1; this.moved = false; this.redraw(); }
    key(e) {
      const n = this.items.length; if (!n) return;
      if ((e.key === "ArrowRight" || e.key === "ArrowLeft") && e.altKey && this.o.reorder) { const j = this.focus + (e.key === "ArrowRight" ? 1 : -1); if (j >= 0 && j < n) { this.o.reorder(this.focus, j); this.focus = j; this.reveal(j); } e.preventDefault(); e.stopPropagation(); }
      else if (e.key === "ArrowRight" || e.key === "ArrowLeft") { this.focus = Math.max(0, Math.min(n - 1, this.focus + (e.key === "ArrowRight" ? 1 : -1))); this.reveal(this.focus); this.redraw(); e.preventDefault(); e.stopPropagation(); }
      else if (e.key === "Enter") { this.o.pick(this.items[this.focus], this.focus); e.preventDefault(); e.stopPropagation(); }
      else if ((e.key === "Delete" || e.key === "Backspace") && this.o.remove) { this.o.remove(this.items[this.focus], this.focus); e.preventDefault(); e.stopPropagation(); }
    }
    draw() {
      const c = this.ctx, d = this.d || 1; if (!this.W) return; c.setTransform(d, 0, 0, d, 0, 0);
      for (const [k, v] of [["lead", "--lead"], ["sup", "--support"], ["pop", "--pop"], ["page", "--page"], ["line", "--line"], ["quiet", "--quiet"], ["bad", "--bad"]]) this.col[k] = cssv(v) || "#888";
      c.fillStyle = this.col.page; c.fillRect(0, 0, this.W, this.H); c.font = '11px "IBM Plex Mono", monospace'; c.textBaseline = "middle";
      const sl = this.host.scrollLeft, i0 = Math.max(0, bisect(this.xs, (v) => v, sl) - 1);
      if (!this.items.length) { c.fillStyle = this.col.quiet; c.fillText(this.o.empty || "", 12, this.H / 2); return; }
      for (let i = i0; i < this.items.length && this.xs[i] - sl < this.W; i++) {
        const x = this.xs[i] - sl, w = this.o.w(this.items[i]);
        this.o.draw(c, this.items[i], i, x, 2, w, this.H - 4, { focus: i === this.focus && document.activeElement === this.cv, sel: this.sel && this.sel(this.items[i]), col: this.col, drag: i === this.dragI && this.moved });
      }
      if (this.dragI >= 0 && this.moved) { const x = (this.dropI > this.dragI ? this.xs[this.dropI + 1] : this.xs[this.dropI]) - sl; c.fillStyle = this.col.pop; c.fillRect(x - 2, 0, 3, this.H); }
    }
  }
  window.WatchTimeline = { Timeline, Rail, fmt, LH, GUT };
})();
