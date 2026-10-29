const $ = Watch.$;
let dirty = false;
let lineTouched = false;
let procMode = "balanced";
let filled = false;
const PRESETS = {
  speed: { blur_on: true, blur: 70, blank_on: true, same_on: true, same: 6, every: 3, width: 416 },
  balanced: { blur_on: true, blur: 40, blank_on: true, same_on: true, same: 3.5, every: 2, width: 512 },
  accuracy: { blur_on: true, blur: 18, blank_on: true, same_on: true, same: 1.2, every: 1, width: 640 }
};

function applyProc(p) {
  if (!p) return;
  procMode = p.mode || "custom";
  $("blur-on").checked = p.blur_on !== false;
  $("blank-on").checked = p.blank_on !== false;
  $("same-on").checked = p.same_on !== false;
  $("blur").value = p.blur ?? 40;
  $("same").value = p.same ?? 3.5;
  $("every").value = String(p.every || 2);
  $("width").value = String(p.width || 512);
  if (p.rate) $("rate").value = String(Number(p.rate) >= 45 ? 60 : 30);
  document.querySelectorAll("[data-preset]").forEach((b) => b.setAttribute("aria-pressed", b.dataset.preset === procMode ? "true" : "false"));
}
function gatherProc() {
  return {
    mode: procMode,
    blur_on: $("blur-on").checked,
    blank_on: $("blank-on").checked,
    same_on: $("same-on").checked,
    blur: Number($("blur").value),
    same: Number($("same").value),
    every: Number($("every").value),
    width: Number($("width").value),
    rate: Number($("rate").value)
  };
}
function fill(s) {
  applyProc(s.process || { mode: "balanced", blur: s.blur, rate: 60 });
  $("rule-list").innerHTML = (s.rules || []).map((r, i) => `<label><input type="checkbox" data-rule="${i}" ${r.enabled ? "checked" : ""}> ${Watch.esc(r.name)} <span class="quiet">${Watch.esc(r.line)}</span></label>`).join("<br>");
  $("mod-list").innerHTML = (s.modules || []).map((m, i) => `<fieldset>
    <label><input type="checkbox" data-on="${i}" ${m.enabled ? "checked" : ""}> ${Watch.esc(m.name)}</label>
    <label>Model path<input data-model="${i}" value="${Watch.esc(m.model || "")}"></label>
    <label>Labels<input data-labels="${i}" value="${Watch.esc((m.labels || []).join(", "))}"></label>
    <label>Confidence<input data-conf="${i}" type="number" step="0.05" min="0" max="1" value="${Watch.num(m.conf)}"></label>
    <label>Rough size, gigaflops<input data-gf="${i}" type="number" step="0.1" min="0" value="${Watch.num(m.gflops)}"></label>
    <label>Hold frames<input data-holdm="${i}" type="number" min="1" value="${m.confirm}"></label>
  </fieldset>`).join("");
}

["r-class", "r-op", "r-n"].forEach((id) => $(id).addEventListener("input", () => {
  if (lineTouched) return;
  $("r-line").value = Watch.lineFor($("r-class").value, $("r-op").value, $("r-n").value);
  $("r-n").disabled = $("r-op").value === "seen";
}));
$("r-line").addEventListener("input", () => { lineTouched = true; dirty = true; });
document.querySelector("main").addEventListener("input", () => { dirty = true; });
$("rule-form").onsubmit = async (e) => {
  e.preventDefault();
  $("r-err").textContent = "";
  const rules = (Watch.state.rules || []).slice();
  const name = $("r-name").value.trim();
  const id = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "rule";
  rules.push({ id, name, camera: $("r-cam").value.trim(), line: $("r-line").value.trim(), hold: Number($("r-hold").value) || 5, enabled: true });
  try { await Watch.post("/api/rules", rules); dirty = false; lineTouched = false; filled = false; }
  catch (err) { $("r-err").textContent = err.message; }
};
$("r-save").onclick = async () => {
  const rules = (Watch.state.rules || []).map((r, i) => ({ ...r, enabled: !!$("rule-list").querySelector(`[data-rule="${i}"]`)?.checked }));
  try { await Watch.post("/api/rules", rules); dirty = false; filled = false; }
  catch (err) { $("r-err").textContent = err.message; }
};
$("mod-form").onsubmit = async (e) => {
  e.preventDefault();
  const modules = (Watch.state.modules || []).map((m, i) => ({
    ...m,
    enabled: !!document.querySelector(`[data-on="${i}"]`)?.checked,
    model: document.querySelector(`[data-model="${i}"]`).value.trim(),
    labels: document.querySelector(`[data-labels="${i}"]`).value.split(",").map((s) => s.trim()).filter(Boolean),
    conf: Number(document.querySelector(`[data-conf="${i}"]`).value),
    gflops: Number(document.querySelector(`[data-gf="${i}"]`).value),
    confirm: Number(document.querySelector(`[data-holdm="${i}"]`).value)
  }));
  try { await Watch.post("/api/modules", modules); dirty = false; filled = false; }
  catch (err) { $("m-err").textContent = err.message; }
};
["blur", "same", "every", "width", "blur-on", "blank-on", "same-on"].forEach((id) => {
  $(id).addEventListener("input", () => {
    procMode = "custom";
    document.querySelectorAll("[data-preset]").forEach((b) => b.setAttribute("aria-pressed", "false"));
  });
});
document.querySelectorAll("[data-preset]").forEach((b) => {
  b.onclick = () => {
    applyProc({ mode: b.dataset.preset, ...PRESETS[b.dataset.preset], rate: Number($("rate").value) });
    dirty = false;
    Watch.post("/api/settings", gatherProc());
  };
});
$("rate").onchange = () => { dirty = false; Watch.post("/api/settings", gatherProc()); };
$("save-set").onclick = () => { dirty = false; Watch.post("/api/settings", gatherProc()); };

Watch.start((s) => {
  $("proc-live").textContent = s.process_line || "";
  if (!filled && !dirty) { fill(s); filled = true; }
}, 1500);
