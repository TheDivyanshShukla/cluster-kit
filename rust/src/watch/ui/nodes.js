// Node status strip. One line to use it on a page that has a query result:
//   Watch.nodes.render(document.getElementById('nodes'), result.nodes);   // result = POST /api/v1/queries response
// Needs <link rel="stylesheet" href="/ui/nodes.css"> and <script src="/ui/nodes.js">. Renders nothing when `nodes` is
// absent (single-store deployment), so it is safe to call always.
(function () {
  const W = (window.Watch = window.Watch || {});
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  function render(el, nodes) {
    if (!el) return;
    if (!Array.isArray(nodes) || !nodes.length) { el.innerHTML = ''; return; }
    const ok = nodes.filter((n) => n.state !== 'down').length;
    const chips = nodes.map((n) =>
      `<span class="wn-chip ${esc(n.state)}" title="${esc(n.error || n.state)}">${esc(n.id.split('|')[0] || n.id)} <small>${n.rows || 0} rows, ${n.elapsed_ms || 0} ms</small></span>`).join('');
    const banner = ok < nodes.length
      ? `<div class="wn-banner">${ok} of ${nodes.length} computers answered; results may be incomplete. Zero results here do not mean none exist.</div>` : '';
    el.innerHTML = `<div class="wn-strip"><span class="wn-sum">${ok}/${nodes.length} computers</span>${chips}${banner}</div>`;
  }
  async function status(el) {
    try {
      const r = await fetch('/api/v1/nodes'); const j = await r.json();
      render(el, (j.nodes || []).map((n) => ({ id: n.id, state: n.state, rows: n.observations, elapsed_ms: 0 })));
    } catch (e) { /* leave as is */ }
  }
  W.nodes = { render, status };
})();
