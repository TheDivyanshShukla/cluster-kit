const $ = Watch.$;
const params = new URLSearchParams(location.search);
const camId = params.get("id") || "";
const startAt = Number(params.get("at") || 0);
const from = Number(params.get("from") || 0);
const to = Number(params.get("to") || 0);
let hits = [];

function pack(items, dur) {
  const rows = [];
  const sorted = items.slice().sort((a, b) => a.at - b.at);
  for (const h of sorted) {
    const left = Math.max(0, Math.min(0.99, h.at / dur));
    const width = Math.max(0.012, Math.min(0.08, 1.4 / dur));
    let row = rows.find((r) => r.every((o) => left >= o.left + o.width + 0.006 || left + width <= o.left - 0.006));
    if (!row) { row = []; rows.push(row); }
    row.push({ h, left: left * 100, width: width * 100 });
  }
  return rows;
}

function paintTracks() {
  const v = $("vid");
  const dur = (v && v.duration) || 0;
  if (!dur) return;
  const by = {};
  for (const h of hits) {
    const k = h.class || "moment";
    (by[k] = by[k] || []).push(h);
  }
  const keys = Object.keys(by);
  $("tracks").innerHTML = keys.map((k) => pack(by[k], dur).map((row, i) => `<div class="lane"><span>${i ? "" : Watch.esc(k)}</span><div class="bed">${row.map((o) => {
    const title = Watch.clipTime(o.h.at) + " " + (o.h.color || "") + " " + k + (o.h.n > 1 ? " ×" + o.h.n : "");
    return `<button type="button" class="strip ${Watch.esc(k)}" style="left:${o.left}%;width:${o.width}%" data-seek="${o.h.at}" title="${Watch.esc(title)}">${Watch.esc(Watch.clipTime(o.h.at))}</button>`;
  }).join("")}</div></div>`).join("")).join("") || `<p class="quiet">No strips in this window.</p>`;
  const marks = [];
  const step = dur > 120 ? 30 : dur > 40 ? 10 : 5;
  for (let t = 0; t <= dur; t += step) marks.push(`<span style="position:absolute;left:${(t / dur) * 100}%">${Watch.clipTime(t)}</span>`);
  $("ruler").innerHTML = marks.join("");
}

function seekTo(at) {
  const v = $("vid");
  if (!v || !Number.isFinite(at)) return;
  const go = () => { v.currentTime = at; paintTracks(); };
  if (v.readyState >= 1) go();
  else v.onloadedmetadata = go;
}

function liveFallback(name) {
  $("monitor").innerHTML = `<img class="live" alt="" src="/api/live/${encodeURIComponent(camId)}">`;
  $("tl").classList.add("hide");
  $("clip-note").textContent = (name || "This camera") + " is a live picture. The file player is for saved footage.";
}

async function loadHits() {
  if (!camId) return;
  try {
    const j = await Watch.post("/api/find", {
      mode: "words", words: "", class: "", color: "", camera: camId, from, to, lua: ""
    });
    hits = Watch.fold(j.hits || []);
    if ($("vid") && $("vid").duration) paintTracks();
  } catch (_) {}
}

function paint(s) {
  const cams = s.cameras || [];
  const key = cams.map((c) => c.id).join(",");
  const film = $("film");
  if (film.dataset.ids !== key) {
    film.dataset.ids = key;
    film.innerHTML = cams.map((c) => `<a class="tile${c.id === camId ? " here" : ""}" href="/clip?id=${encodeURIComponent(c.id)}">
      <div class="stage-view"><img alt="" src="/api/live/${encodeURIComponent(c.id)}"><svg></svg></div>
      <div class="meta"><b></b><span class="pill"></span></div></a>`).join("");
  }
  for (const c of cams) {
    const node = [...film.querySelectorAll("a.tile")].find((a) => new URL(a.href).searchParams.get("id") === c.id);
    if (!node) continue;
    node.querySelector("b").textContent = c.name;
    node.querySelector(".pill").textContent = [c.feed, c.status].filter(Boolean).join(" · ");
    Watch.boxes(node.querySelector("svg"), c.boxes);
  }
  const cam = cams.find((c) => c.id === camId);
  if (cam) $("ed-name").textContent = cam.name;
}

$("tracks").onclick = (e) => {
  const t = e.target.dataset && e.target.dataset.seek;
  if (t == null || t === "") return;
  seekTo(Number(t));
  const q = new URLSearchParams(location.search);
  q.set("at", t);
  history.replaceState(null, "", "/clip?" + q.toString());
};
$("ruler").onclick = (e) => {
  const v = $("vid");
  if (!v || !v.duration) return;
  const box = $("ruler").getBoundingClientRect();
  seekTo(Math.max(0, Math.min(v.duration, ((e.clientX - box.left) / box.width) * v.duration)));
};

function follow() {
  const v = $("vid");
  const sheet = $("sheet");
  if (v && sheet && v.duration) {
    const bed = sheet.querySelector(".bed");
    const base = bed ? bed.offsetLeft : 116;
    const width = bed ? bed.clientWidth : Math.max(1, sheet.clientWidth - base);
    $("playhead").style.left = (base + (v.currentTime / v.duration) * width) + "px";
    $("ed-clock").textContent = Watch.clipTime(v.currentTime);
  }
  requestAnimationFrame(follow);
}
follow();

if (!camId) {
  $("monitor").innerHTML = `<p class="empty">Pick a camera from the wall.</p>`;
  $("tl").classList.add("hide");
} else {
  const v = $("vid");
  v.src = "/api/media/" + encodeURIComponent(camId);
  v.addEventListener("loadedmetadata", () => {
    if (startAt) v.currentTime = startAt;
    paintTracks();
  });
  v.addEventListener("error", () => liveFallback($("ed-name").textContent));
  loadHits();
}
Watch.start(paint, 400);
