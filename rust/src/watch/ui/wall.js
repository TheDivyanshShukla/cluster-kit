const $ = Watch.$;

function paint(s) {
  $("sentence").textContent = s.sentence || "";
  $("foot").textContent = s.footnote || "";
  $("mode-one").setAttribute("aria-pressed", s.solo ? "true" : "false");
  $("mode-all").setAttribute("aria-pressed", s.solo ? "false" : "true");
  $("proc-live").textContent = s.process_line || "";
  $("warnings").innerHTML = (s.warnings || []).map((w) => `<div class="warn">${Watch.esc(w)}</div>`).join("");
  const tiles = $("tiles");
  const cams = s.cameras || [];
  const empty = $("wall-empty");
  if (!s.ffmpeg) {
    empty.classList.remove("hide");
    empty.textContent = "ffmpeg is not on PATH. Install it, then start Watch again.";
  } else if (!cams.length) {
    empty.classList.remove("hide");
    empty.textContent = "No cameras yet. On each PC run kit rs watch fetch, or set an RTSP address in vision/watch.json.";
  } else empty.classList.add("hide");
  const ids = cams.map((c) => c.id).join(",");
  if (tiles.dataset.ids !== ids) {
    tiles.dataset.ids = ids;
    tiles.innerHTML = cams.map((c) => `<a class="tile" href="/clip?id=${encodeURIComponent(c.id)}">
      <div class="stage-view"><img alt="" src="/api/live/${encodeURIComponent(c.id)}"><svg></svg></div>
      <div class="meta"><b></b><span class="pill"></span></div></a>`).join("");
  }
  for (const c of cams) {
    const node = [...tiles.querySelectorAll("a.tile")].find((a) => new URL(a.href).searchParams.get("id") === c.id);
    if (!node) continue;
    node.querySelector("b").textContent = c.name;
    node.querySelector(".pill").textContent = [c.feed, c.status, c.people, c.skip].filter(Boolean).join(" · ");
    Watch.boxes(node.querySelector("svg"), c.boxes);
  }
  const events = s.events || [];
  $("events").innerHTML = events.map((e) => `<div class="hit">
    <span class="when">${Watch.esc(e.status)}</span>
    <span>${Watch.esc(e.rule)}<br><span class="quiet">${Watch.esc(e.camera)} ${Watch.esc(e.note || "")}</span></span>
    <span>${e.status === "agreed" ? `<button type="button" data-yes="${e.id}">Yes</button> <button type="button" data-no="${e.id}">No</button>` : ""}</span>
  </div>`).join("") || `<p class="quiet">No checks yet.</p>`;
}

$("events").onclick = (e) => {
  const yes = e.target.dataset.yes, no = e.target.dataset.no;
  if (yes) Watch.post("/api/confirm", { id: Number(yes), verdict: "confirm" });
  if (no) Watch.post("/api/confirm", { id: Number(no), verdict: "reject" });
};
$("mode-one").onclick = () => Watch.post("/api/settings", { solo: true });
$("mode-all").onclick = () => Watch.post("/api/settings", { solo: false });
Watch.start(paint, 400);
