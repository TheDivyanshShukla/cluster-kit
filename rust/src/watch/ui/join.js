(() => {
  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  let info = null, code = null, tick = null;

  async function j(url, opt) {
    const r = await fetch(url, Object.assign({ credentials: "same-origin" }, opt));
    const body = await r.json().catch(() => ({}));
    return { ok: r.ok, status: r.status, body };
  }

  function head() {
    const local = /^(localhost|127\.|\[::1\])/.test(location.hostname);
    if (local && info && info.lan_ip) return "http://" + info.lan_ip + ":" + (location.port || "7710");
    return location.origin;
  }

  function paint() {
    const h = head();
    const q = code && Date.now() / 1000 < code.expires ? "?code=" + code.code : "";
    $("cmd-sh").textContent = `curl -fsSL "${h}/api/v1/join/install.sh${q}" | sh`;
    $("cmd-ps").textContent = `irm "${h}/api/v1/join/install.ps1${q}" | iex`;
    const ip = h.replace(/^https?:\/\//, "");
    $("cmd-cw").textContent = `cw join ${ip.split(":")[0]}${ip.includes(":") && ip.split(":")[1] !== "7710" ? " --port " + ip.split(":")[1] : ""}${code && q ? " --code " + code.code : ""}`;
    $("deploy-head").textContent = ip.split(":")[0];
    const warn = $("localhost");
    if (/^(localhost|127\.|\[::1\])/.test(location.hostname)) {
      warn.hidden = false;
      warn.textContent = info && info.lan_ip
        ? "You opened this page as " + location.host + ". The commands below use this PC's network address " + info.lan_ip + " instead."
        : "You opened this page as " + location.host + ". Open it by this PC's network address so the commands work from other PCs.";
    } else warn.hidden = true;
  }

  function showCode() {
    const el = $("code"), meta = $("code-meta");
    if (!code) { el.textContent = "----"; el.classList.add("dead"); meta.textContent = info && !info.needs_code ? "Open mode: no code is needed on a trusted network. A code is optional." : ""; return; }
    const left = Math.round(code.expires - Date.now() / 1000);
    el.classList.toggle("dead", left <= 0);
    el.textContent = code.code;
    meta.textContent = left > 0 ? `expires in ${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")} - admits ${code.uses} PC${code.uses === 1 ? "" : "s"}` : "expired - make a new one";
    paint();
  }

  async function loadCodes() {
    const r = await j("/api/v1/join/codes");
    if (r.ok) {
      const list = (r.body.codes || []).sort((a, b) => b.expires - a.expires);
      if (list.length) code = { code: list[0].code, expires: list[0].expires, uses: list[0].uses_left };
    } else if (r.status === 401 || r.status === 403) {
      $("pair-msg").className = "msg bad";
      $("pair-msg").innerHTML = 'Only an administrator can make pairing codes. <a class="text-link" href="/login?next=/join">Sign in</a>';
      $("mk").disabled = true;
    }
  }

  $("mk").addEventListener("click", async () => {
    const msg = $("pair-msg");
    msg.className = "msg"; msg.textContent = "";
    const r = await j("/api/v1/join/codes", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ uses: Number($("uses").value) || 1 }) });
    if (!r.ok) { msg.className = "msg bad"; msg.textContent = (r.body.error && r.body.error.message) || "Could not make a code"; return; }
    code = { code: r.body.code, expires: r.body.expires, uses: r.body.uses };
    showCode();
  });

  document.addEventListener("click", async (e) => {
    const b = e.target.closest("[data-copy]");
    if (!b) return;
    const text = $(b.dataset.copy).textContent;
    try { await navigator.clipboard.writeText(text); b.textContent = "Copied"; }
    catch { const r = document.createRange(); r.selectNodeContents($(b.dataset.copy)); const s = getSelection(); s.removeAllRanges(); s.addRange(r); b.textContent = "Select + copy"; }
    setTimeout(() => (b.textContent = "Copy"), 1500);
  });

  const when = (t) => (t ? new Date(t * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "");
  async function nodes() {
    const r = await j("/api/v1/cluster");
    const body = document.querySelector("#nodes tbody");
    if (!r.ok) {
      $("nodes-msg").className = "msg quiet";
      $("nodes-msg").innerHTML = r.status === 401 ? 'Sign in to see the PCs. <a class="text-link" href="/login?next=/join">Sign in</a>' : "Could not read the cluster.";
      body.innerHTML = ""; return;
    }
    $("nodes-msg").textContent = "";
    const list = r.body.nodes || [];
    body.innerHTML = list.map((n) => {
      const cls = n.state === "lost" ? "bad" : n.state === "slow" ? "warn" : "ok";
      return `<tr class="${n.state === "lost" ? "lost" : ""}"><td>${esc(n.label || n.host)}${n.local ? ' <span class="tag">head</span>' : ""}</td><td>${esc(n.os)} ${esc(n.arch)}</td><td class="n">${n.cores}</td><td><span class="state"><i class="dot ${cls}"></i>${esc(n.state)}</span></td><td>${esc(when(n.joined))}</td><td class="n">${n.measured_fps ?? ""}</td><td class="n">${Math.round(n.cpu || 0)}%</td></tr>`;
    }).join("") || `<tr><td colspan="7" class="quiet">No PC has reported yet.</td></tr>`;
  }

  (async () => {
    const r = await j("/api/v1/join/info");
    info = r.body || {};
    $("mode-line").textContent = info.needs_code
      ? "Sign-in is on: a PC needs a valid pairing code, and only an administrator can make one."
      : "Open mode: anyone on the network can join a PC. Add an administrator (Setup) to require pairing codes.";
    await loadCodes();
    showCode(); paint();
    await nodes();
    tick = setInterval(() => { showCode(); }, 1000);
    setInterval(nodes, 3000);
  })();
})();
