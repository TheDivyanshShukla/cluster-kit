const $ = Watch.$;
let findMode = "words";
let savedKey = "";

function setFindMode(mode) {
  findMode = mode;
  document.querySelectorAll("#find-modes button").forEach((b) => b.setAttribute("aria-pressed", b.dataset.mode === mode ? "true" : "false"));
  $("mode-words").classList.toggle("hide", mode !== "words");
  $("mode-template").classList.toggle("hide", mode !== "template");
  $("mode-lua").classList.toggle("hide", mode !== "lua");
}
function askBody() {
  return {
    mode: findMode,
    words: $("q").value,
    class: $("t-class").value,
    color: $("t-color").value,
    camera: findMode === "template" ? $("t-cam").value : "",
    from: Watch.fromLocal($("from").value),
    to: Watch.fromLocal($("to").value),
    lua: $("lua").value
  };
}
function clipHref(h) {
  const q = new URLSearchParams();
  q.set("id", h.camera_id || "");
  q.set("at", String(h.at || 0));
  const from = Watch.fromLocal($("from").value);
  const to = Watch.fromLocal($("to").value);
  if (from) q.set("from", String(from));
  if (to) q.set("to", String(to));
  return "/clip?" + q.toString();
}
function showHits(hits, error) {
  $("find-err").textContent = error || "";
  const list = Watch.fold(hits || []);
  $("hit-count").textContent = list.length ? list.length + " moments" : (error ? "" : "No moments in that window.");
  $("hits").innerHTML = list.map((h) => `<a class="hit" href="${Watch.esc(clipHref(h))}">
    <img alt="" src="/api/hit/${h.id}">
    <span>${Watch.esc([h.color, h.class].filter(Boolean).join(" "))}<br><span class="quiet">${Watch.esc(h.camera)}</span></span>
    <span class="when">${Watch.esc(Watch.when(h.ts))}<br>${Watch.esc(Watch.clipTime(h.at))}${h.n > 1 ? " ×" + h.n : ""}</span>
  </a>`).join("");
}
async function runFind() {
  $("find-err").textContent = "";
  try {
    const j = await Watch.post("/api/find", askBody());
    showHits(j.hits, j.error);
  } catch (err) { $("find-err").textContent = err.message; }
}
function paintSaved(list) {
  $("saved").innerHTML = list.map((s, i) => `<button type="button" class="saved" data-saved="${i}">
    <span class="when">${Watch.esc(s.mode || "words")}</span>
    <span>${Watch.esc(s.name)}<br><span class="quiet">${Watch.esc(s.words || s.class || s.lua || "").slice(0, 80)}</span></span>
    <span data-drop="${i}">Remove</span>
  </button>`).join("") || `<p class="quiet">No saved searches yet.</p>`;
}

$("find-modes").onclick = (e) => { const b = e.target.closest("button"); if (b) setFindMode(b.dataset.mode); };
$("ask").onsubmit = (e) => { e.preventDefault(); runFind(); };
$("saved").onclick = async (e) => {
  const drop = e.target.dataset.drop;
  if (drop != null && drop !== "") {
    e.preventDefault();
    const list = (Watch.state.searches || []).slice();
    list.splice(Number(drop), 1);
    await Watch.post("/api/searches", list);
    return;
  }
  const b = e.target.closest("[data-saved]");
  if (!b || !Watch.state) return;
  const s = Watch.state.searches[Number(b.dataset.saved)];
  if (!s) return;
  setFindMode(s.mode || "words");
  $("q").value = s.words || "";
  $("t-class").value = s.class || "";
  $("t-color").value = s.color || "";
  $("t-cam").value = s.camera || "";
  $("from").value = Watch.toLocal(s.from);
  $("to").value = Watch.toLocal(s.to);
  if (s.lua) $("lua").value = s.lua;
  runFind();
};
$("save-search").onclick = async () => {
  const name = $("search-name").value.trim() || "Search";
  const id = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "search";
  const list = (Watch.state && Watch.state.searches || []).concat([{ id, name, ...askBody() }]);
  await Watch.post("/api/searches", list);
  $("search-name").value = "";
};

Watch.start((s) => {
  const key = JSON.stringify(s.searches || []);
  if (key !== savedKey) { savedKey = key; paintSaved(s.searches || []); }
}, 1500);
