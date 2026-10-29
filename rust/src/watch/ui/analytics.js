const $ = Watch.$;

function draw(s) {
  const c = $("trend");
  if (!c) return;
  const w = Math.max(320, Math.floor(c.clientWidth || 900));
  if (c.width !== w) c.width = w;
  const g = c.getContext("2d");
  const h = c.height;
  g.clearRect(0, 0, c.width, h);
  const hist = s.history || [];
  const line = (key, color) => {
    const vals = hist.map((p) => Number(p[key]) || 0);
    const max = Math.max(1, ...vals);
    g.strokeStyle = color;
    g.lineWidth = 2;
    g.beginPath();
    vals.forEach((v, i) => {
      const x = vals.length < 2 ? 8 : (i / (vals.length - 1)) * (c.width - 16) + 8;
      const y = h - 12 - (v / max) * (h - 24);
      if (i === 0) g.moveTo(x, y); else g.lineTo(x, y);
    });
    g.stroke();
  };
  line("lacking", "#CE7D36");
  line("ops", "#326E6E");
}

function paint(s) {
  $("sentence").textContent = s.sentence || "";
  $("foot").textContent = s.footnote || "";
  draw(s);
  const cams = s.cameras || [];
  const maxL = Math.max(1, ...cams.map((c) => Number(String(c.lacking).replace(/[^\d.]/g, "")) || 0));
  $("lack-bars").innerHTML = cams.map((c) => {
    const n = Number(String(c.lacking).replace(/[^\d.]/g, "")) || 0;
    return `<div class="bar lack"><span>${Watch.esc(c.name)}</span><i style="width:${Math.max(2, (n / maxL) * 100)}%"></i><span class="mono">${Watch.esc(c.lacking)}</span></div>`;
  }).join("") || `<p class="quiet">No cameras reporting.</p>`;
  const mix = {};
  for (const h of (s.seen || [])) mix[h.class] = (mix[h.class] || 0) + 1;
  const entries = Object.entries(mix).sort((a, b) => b[1] - a[1]);
  const maxM = Math.max(1, ...entries.map((e) => e[1]));
  $("mix").innerHTML = entries.map(([k, n]) => `<div class="bar"><span>${Watch.esc(k)}</span><i style="width:${(n / maxM) * 100}%"></i><span class="mono">${n}</span></div>`).join("") || `<p class="quiet">Nothing logged yet.</p>`;
  const rows = s.rows || [];
  const headers = s.headers || [];
  $("matrix").innerHTML = `<thead><tr><th></th>${headers.map((h) => `<th title="${Watch.esc(h)}">${Watch.esc(h)}</th>`).join("")}</tr></thead><tbody>`
    + rows.map((r) => `<tr><td>${Watch.esc(r.label)}</td>${(r.cells || []).map((c) => `<td>${Watch.esc(c)}</td>`).join("")}</tr>`).join("")
    + "</tbody>";
}

Watch.start(paint, 1000);
