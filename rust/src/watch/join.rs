//! Putting another PC into the cluster, only when someone on that PC (or its ssh owner) asks for it.
//!
//! Head side: pairing codes, the install scripts, a prebuilt bundle of the head's own program, the source, the
//! model manifest. PC side: `cw join` (enroll, fetch models, run the worker) and `cw deploy user@host` (ssh push).
//! Nothing here installs or starts anything on a machine nobody named.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::watch::api::{param, Api, Reply};
use crate::watch::domain::ApiError;
use crate::watch::{config, log, now_secs, repo_root, Opts, PORT};

/// Pairing codes live this long.
pub const CODE_SECS: f64 = 600.0;
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// Source bundle ceiling. The repo's Rust sources are a few MB; anything near this means target dirs leaked in.
const SOURCE_CAP: u64 = 60 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Code {
    pub code: String,
    pub expires: f64,
    pub uses_left: u32,
    pub uses: u32,
}

#[derive(Default)]
pub struct State {
    pub codes: Vec<Code>,
    fails: VecDeque<f64>,
    hashes: std::collections::HashMap<PathBuf, (u64, std::time::SystemTime, String)>,
}

fn norm(c: &str) -> String {
    c.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase()
}

fn err(code: &str, m: impl Into<String>) -> ApiError {
    ApiError::new(code, m)
}

impl State {
    /// A fresh code, valid for [`CODE_SECS`] and `uses` enrollments.
    pub fn create(&mut self, uses: u32, now: f64) -> Result<Code, ApiError> {
        self.codes.retain(|c| c.expires > now && c.uses_left > 0);
        let raw = crate::watch::auth::random(8)?;
        let s: String = raw.iter().map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char).collect();
        let c = Code { code: format!("{}-{}", &s[..4], &s[4..]), expires: now + CODE_SECS, uses_left: uses.clamp(1, 50), uses: 0 };
        self.codes.push(c.clone());
        Ok(c)
    }

    /// Is `code` valid right now? `consume` spends one use. Ten wrong guesses a minute lock the door for that minute.
    pub fn check(&mut self, code: &str, consume: bool, now: f64) -> Result<(), ApiError> {
        while self.fails.front().is_some_and(|t| now - t > 60.0) {
            self.fails.pop_front();
        }
        if self.fails.len() >= 10 {
            return Err(err("rate_limited", "too many wrong pairing codes; wait a minute"));
        }
        let want = norm(code);
        let hit = self.codes.iter_mut().find(|c| c.expires > now && c.uses_left > 0 && norm(&c.code) == want && !want.is_empty());
        match hit {
            Some(c) => {
                if consume {
                    c.uses_left -= 1;
                    c.uses += 1;
                }
                Ok(())
            }
            None => {
                self.fails.push_back(now);
                Err(err("unauthorized", "that pairing code is wrong, used up or expired; make a new one on the /join page"))
            }
        }
    }
}

// ---------------------------------------------------------------- head side

fn open(api: &Api) -> bool {
    api.auth.mode() == "open-trusted-lan"
}

fn safe_host(h: &str) -> bool {
    !h.is_empty() && h.len() < 100 && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '[' | ']'))
}

/// `host:port` the PC reached the head by, from its Host header (strictly filtered: it ends up inside a shell script).
fn head_addr(headers: &[(String, String)]) -> String {
    let h = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("host")).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
    if safe_host(&h) { h } else { format!("{}:{PORT}", crate::watch::primary_ip()) }
}

fn script_code(api: &Api, url: &str) -> Result<String, ApiError> {
    let code = param(url, "code").unwrap_or_default();
    if !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') || code.len() > 24 {
        return Err(err("invalid_request", "bad pairing code"));
    }
    if !open(api) {
        api.join.lock().unwrap().check(&code, false, now_secs())?;
    }
    Ok(code)
}

fn text(body: String, ctype: &'static str) -> Reply {
    Reply { status: 200, ctype, body: body.into_bytes(), headers: Vec::new() }
}

fn file_reply(path: &Path, ctype: &'static str, name: &str) -> Result<Reply, ApiError> {
    let body = std::fs::read(path).map_err(|e| err("unavailable", format!("{}: {e}", path.display())))?;
    Ok(Reply { status: 200, ctype, body, headers: vec![("Content-Disposition".into(), format!("attachment; filename=\"{name}\""))] })
}

fn sha_of(api: &Api, path: &Path) -> Result<String, ApiError> {
    let meta = std::fs::metadata(path).map_err(|e| err("not_found", format!("{}: {e}", path.display())))?;
    let key = (meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH));
    if let Some((len, mt, h)) = api.join.lock().unwrap().hashes.get(path) {
        if (*len, *mt) == key {
            return Ok(h.clone());
        }
    }
    let h = sha256_file(path).map_err(|e| err("unavailable", e))?;
    api.join.lock().unwrap().hashes.insert(path.to_path_buf(), (key.0, key.1, h.clone()));
    Ok(h)
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn os_arch() -> (&'static str, &'static str) {
    (std::env::consts::OS, std::env::consts::ARCH)
}

/// The head's own program plus any runtime libraries beside it, as a tarball. Only for the head's own OS and CPU.
fn bundle(api: &Api, os: &str, arch: &str) -> Result<PathBuf, ApiError> {
    let (my_os, my_arch) = os_arch();
    if (os, arch) != (my_os, my_arch) {
        return Err(err(
            "not_found",
            format!("no prebuilt program for {os}/{arch}: this head is {my_os}/{my_arch}. Build from source on that PC (the installer does it when Rust is installed)."),
        ));
    }
    let exe = std::env::current_exe().map_err(|e| err("unavailable", e.to_string()))?;
    let dir = api.data_dir.join("join");
    std::fs::create_dir_all(&dir).map_err(|e| err("unavailable", e.to_string()))?;
    let out = dir.join(format!("bundle-{os}-{arch}.tar.gz"));
    let fresh = std::fs::metadata(&out).and_then(|o| o.modified()).ok().zip(std::fs::metadata(&exe).and_then(|e| e.modified()).ok()).is_some_and(|(o, e)| o >= e);
    if !fresh {
        let stage = dir.join(format!("stage-{os}-{arch}"));
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(&stage).map_err(|e| err("unavailable", e.to_string()))?;
        let name = if cfg!(windows) { "cw.exe" } else { "cw" };
        std::fs::copy(&exe, stage.join(name)).map_err(|e| err("unavailable", e.to_string()))?;
        // ONNX Runtime libraries that the build copied next to the program (absent when it is linked statically).
        if let Some(parent) = exe.parent() {
            for e in std::fs::read_dir(parent).into_iter().flatten().flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.contains("onnxruntime") && (n.ends_with(".dylib") || n.contains(".so") || n.ends_with(".dll")) {
                    let _ = std::fs::copy(e.path(), stage.join(&n));
                }
            }
        }
        let st = Command::new("tar").arg("czf").arg(&out).arg("-C").arg(&stage).arg(".").status().map_err(|e| err("unavailable", format!("tar: {e}")))?;
        let _ = std::fs::remove_dir_all(&stage);
        if !st.success() {
            return Err(err("unavailable", "tar failed while packing the program"));
        }
    }
    Ok(out)
}

/// The Rust sources (no build output), for a PC whose OS differs from the head's.
fn source(api: &Api) -> Result<PathBuf, ApiError> {
    let rust = repo_root().join("rust");
    if !rust.join("Cargo.toml").is_file() || !rust.join("src").is_dir() {
        return Err(err("not_found", format!("the head has no rust/ sources under {}", repo_root().display())));
    }
    let dir = api.data_dir.join("join");
    std::fs::create_dir_all(&dir).map_err(|e| err("unavailable", e.to_string()))?;
    let out = dir.join("source.tar.gz");
    let newest = |p: &Path| -> std::time::SystemTime {
        let mut best = std::time::UNIX_EPOCH;
        let mut stack = vec![p.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let m = e.metadata();
                if let Ok(m) = m {
                    if m.is_dir() { stack.push(e.path()) } else if let Ok(t) = m.modified() { best = best.max(t) }
                }
            }
        }
        best
    };
    let stamp = newest(&rust.join("src")).max(std::fs::metadata(rust.join("Cargo.toml")).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH));
    if !std::fs::metadata(&out).and_then(|m| m.modified()).is_ok_and(|t| t >= stamp) {
        let mut cmd = Command::new("tar");
        cmd.arg("czf").arg(&out).args(["--exclude", "target", "--exclude", "target-*", "--exclude", ".DS_Store", "-C"]).arg(&rust).args(["Cargo.toml", "Cargo.lock", "src"]);
        let st = cmd.status().map_err(|e| err("unavailable", format!("tar: {e}")))?;
        if !st.success() {
            return Err(err("unavailable", "tar failed while packing the source"));
        }
    }
    let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    if len > SOURCE_CAP {
        let _ = std::fs::remove_file(&out);
        return Err(err("too_large", format!("the source bundle is {} MB, over the {} MB cap; something other than sources is under rust/", len >> 20, SOURCE_CAP >> 20)));
    }
    Ok(out)
}

#[derive(serde::Serialize)]
struct FileInfo {
    path: String,
    size: u64,
    sha256: String,
}

/// The files a joining PC needs: the model of every enabled detector module that exists on the head.
fn model_files(api: &Api) -> Vec<FileInfo> {
    let root = repo_root();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in config::load_modules().into_iter().filter(|m| m.enabled && m.kind != "fight" && !m.model.is_empty()) {
        let rel = m.model.replace('\\', "/");
        if rel.starts_with('/') || rel.split('/').any(|s| s == "..") || !seen.insert(rel.clone()) {
            continue;
        }
        let p = root.join(&rel);
        if let (Ok(meta), Ok(sha)) = (std::fs::metadata(&p), sha_of(api, &p)) {
            out.push(FileInfo { path: rel, size: meta.len(), sha256: sha });
        }
    }
    out
}

fn sanitize_name(n: &str) -> String {
    let s: String = n.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).take(40).collect();
    if s.is_empty() { "pc".into() } else { s }
}

#[derive(serde::Deserialize, Default)]
struct EnrollIn {
    #[serde(default)]
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    arch: String,
}

fn enroll(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let r: EnrollIn = serde_json::from_slice(body).map_err(|e| err("parse_error", e.to_string()))?;
    if !open(api) {
        api.join.lock().unwrap().check(&r.code, true, now_secs())?;
    }
    let name = sanitize_name(if r.name.is_empty() { &r.host } else { &r.name });
    let token = api.auth.create_worker_token(&name)?;
    crate::watch::api::audit(api, None, "cluster.enroll", &[&name], "ok", &format!("{} {}/{} joined", r.host, r.os, r.arch));
    log(&format!("enrolled {name} ({} {}/{})", r.host, r.os, r.arch));
    let w = config::load_watch();
    Ok(Reply::json(&serde_json::json!({ "worker_token": token, "name": name, "mode": api.auth.mode(), "process": w.process, "note": "shown once; only a hash is stored" })))
}

fn manifest(api: &Api) -> Result<Reply, ApiError> {
    Ok(Reply::json(&serde_json::json!({
        "modules": config::load_modules(), "rules": config::load_rules(), "process": config::load_watch().process,
        "files": model_files(api),
    })))
}

fn model_file(api: &Api, url: &str) -> Result<Reply, ApiError> {
    let rel = param(url, "path").ok_or_else(|| err("invalid_request", "path required"))?;
    if !model_files(api).iter().any(|f| f.path == rel) {
        return Err(err("not_found", "that file is not part of the worker manifest"));
    }
    file_reply(&repo_root().join(&rel), "application/octet-stream", rel.rsplit('/').next().unwrap_or("file"))
}

/// All `/api/v1/join/*` routes. `None` = not ours.
pub fn handle(api: &Api, method: &str, url: &str, body: &[u8], headers: &[(String, String)]) -> Option<Reply> {
    let path = url.split('?').next().unwrap_or(url);
    let rest = path.strip_prefix("/api/v1/join/")?;
    let addr = head_addr(headers);
    let r: Result<Reply, ApiError> = (|| match (method, rest) {
        ("GET", "info") => {
            let (os, arch) = os_arch();
            Ok(Reply::json(&serde_json::json!({
                "head": format!("http://{addr}"), "lan_ip": crate::watch::primary_ip(), "mode": api.auth.mode(), "needs_code": !open(api),
                "bundle": { "os": os, "arch": arch }, "source": repo_root().join("rust/Cargo.toml").is_file(),
                "code_minutes": (CODE_SECS / 60.0) as u32,
            })))
        }
        ("GET", "install.sh") => {
            let code = script_code(api, url)?;
            Ok(text(INSTALL_SH.replace("__HEAD__", &format!("http://{addr}")).replace("__ADDR__", &addr).replace("__CODE__", &code), "text/x-shellscript; charset=utf-8"))
        }
        ("GET", "install.ps1") => {
            let code = script_code(api, url)?;
            Ok(text(INSTALL_PS1.replace("__HEAD__", &format!("http://{addr}")).replace("__ADDR__", &addr).replace("__CODE__", &code), "text/plain; charset=utf-8"))
        }
        ("GET", "source.tar.gz") => {
            script_code(api, url)?;
            file_reply(&source(api)?, "application/gzip", "watch-source.tar.gz")
        }
        ("GET", p) if p.starts_with("bundle/") => {
            script_code(api, url)?;
            let name = &p["bundle/".len()..];
            let (sha, stem) = match name.strip_suffix(".sha256") {
                Some(s) => (true, s),
                None => (false, name),
            };
            let target = stem.strip_suffix(".tar.gz").ok_or_else(|| err("not_found", "expected bundle/<os>-<arch>.tar.gz"))?;
            let (os, arch) = target.split_once('-').ok_or_else(|| err("not_found", "expected <os>-<arch>"))?;
            let file = bundle(api, os, arch)?;
            if sha {
                Ok(text(format!("{}  {stem}\n", sha_of(api, &file)?), "text/plain; charset=utf-8"))
            } else {
                file_reply(&file, "application/gzip", stem)
            }
        }
        ("POST", "enroll") => enroll(api, body),
        ("POST", "codes") => {
            let uses = serde_json::from_slice::<serde_json::Value>(body).ok().and_then(|v| v["uses"].as_u64()).unwrap_or(1) as u32;
            let c = api.join.lock().unwrap().create(uses, now_secs())?;
            Ok(Reply::json(&serde_json::json!({ "code": c.code, "expires": c.expires, "expires_in": CODE_SECS, "uses": c.uses_left })))
        }
        ("GET", "codes") => {
            let now = now_secs();
            let mut j = api.join.lock().unwrap();
            j.codes.retain(|c| c.expires > now && c.uses_left > 0);
            Ok(Reply::json(&serde_json::json!({ "codes": j.codes.iter().map(|c| serde_json::json!({"code": c.code, "expires": c.expires, "uses_left": c.uses_left})).collect::<Vec<_>>() })))
        }
        ("GET", "manifest") => manifest(api),
        ("GET", "file") => model_file(api, url),
        _ => Err(err("not_found", "no such join route")),
    })();
    Some(r.unwrap_or_else(|e| {
        let mut out = Reply::err(e.clone());
        if e.code == "rate_limited" {
            out.status = 429;
        }
        out
    }))
}

const INSTALL_SH: &str = r##"#!/bin/sh
# Watch worker installer, generated by the head at __HEAD__.
# It puts one program in ~/.watch-worker and starts it. It changes nothing else on this PC.
set -eu
HEAD="__HEAD__"; ADDR="__ADDR__"; CODE="__CODE__"
HOST="${ADDR%:*}"; PORT="${ADDR##*:}"
DEST="${CW_HOME:-$HOME/.watch-worker}"
say() { printf '%s\n' "watch-worker: $*"; }
die() { printf '%s\n' "watch-worker: $*" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || die "curl is needed"
command -v tar >/dev/null 2>&1 || die "tar is needed"
command -v ffmpeg >/dev/null 2>&1 || say "warning: ffmpeg is not on PATH and the worker needs it (macOS: brew install ffmpeg; Debian/Ubuntu: sudo apt install ffmpeg)"
case "$(uname -s)" in Darwin) OS=macos;; Linux) OS=linux;; *) die "unsupported OS $(uname -s); on Windows use install.ps1";; esac
case "$(uname -m)" in arm64|aarch64) ARCH=aarch64;; x86_64|amd64) ARCH=x86_64;; *) die "unsupported CPU $(uname -m)";; esac
mkdir -p "$DEST"; cd "$DEST"
Q=""; [ -n "$CODE" ] && Q="?code=$CODE"
say "head $HEAD, this PC is $OS/$ARCH, installing into $DEST"
got=""
if curl -fsS -o bundle.tgz "$HEAD/api/v1/join/bundle/$OS-$ARCH.tar.gz$Q" 2>bundle.err; then
  want=$(curl -fsS "$HEAD/api/v1/join/bundle/$OS-$ARCH.tar.gz.sha256$Q" | cut -d' ' -f1)
  if command -v shasum >/dev/null 2>&1; then have=$(shasum -a 256 bundle.tgz | cut -d' ' -f1); else have=$(sha256sum bundle.tgz | cut -d' ' -f1); fi
  [ -n "$want" ] && [ "$want" = "$have" ] || die "checksum mismatch on the downloaded program; not installing"
  tar xzf bundle.tgz && chmod +x cw && got=1 && say "prebuilt program installed, checksum ok"
else
  say "no prebuilt program for $OS/$ARCH: $(tr -d '\n' < bundle.err | cut -c1-300)"
fi
if [ -z "$got" ]; then
  command -v cargo >/dev/null 2>&1 || die "this needs Rust to build from source: https://rustup.rs (then run this command again)"
  say "building from the head's source (first build takes several minutes)"
  curl -fsS -o source.tgz "$HEAD/api/v1/join/source.tar.gz$Q" || die "could not download the source"
  rm -rf build && mkdir build && tar xzf source.tgz -C build
  FEAT=watch; [ "$OS" = macos ] && FEAT=watch,coreml
  (cd build && cargo build --release --no-default-features --features "$FEAT") || die "build failed"
  cp build/target/release/cw ./cw
  say "built from source"
fi
NAME="$(hostname -s 2>/dev/null || hostname)"
if [ "${CW_DETACH:-}" = 1 ]; then
  nohup ./cw join "$HOST" --port "$PORT" ${CODE:+--code "$CODE"} --name "$NAME" >worker.log 2>&1 &
  say "worker started in the background (log: $DEST/worker.log)"
else
  say "starting the worker; press Ctrl-C to leave the cluster"
  exec ./cw join "$HOST" --port "$PORT" ${CODE:+--code "$CODE"} --name "$NAME"
fi
"##;

const INSTALL_PS1: &str = r##"# Watch worker installer, generated by the head at __HEAD__.
# It puts one program in %USERPROFILE%\.watch-worker and starts it. It changes nothing else on this PC.
$ErrorActionPreference = 'Stop'
$Head = '__HEAD__'; $Addr = '__ADDR__'; $Code = '__CODE__'
$HostName = $Addr.Substring(0, $Addr.LastIndexOf(':')); $Port = $Addr.Substring($Addr.LastIndexOf(':') + 1)
$Dest = Join-Path $env:USERPROFILE '.watch-worker'
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Set-Location $Dest
if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) { Write-Host 'watch-worker: warning: ffmpeg is not on PATH and the worker needs it (winget install ffmpeg)' }
$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$Q = if ($Code) { "?code=$Code" } else { '' }
Write-Host "watch-worker: head $Head, this PC is windows/$Arch, installing into $Dest"
$got = $false
try {
  Invoke-WebRequest -UseBasicParsing -OutFile bundle.tgz "$Head/api/v1/join/bundle/windows-$Arch.tar.gz$Q"
  $want = ((Invoke-WebRequest -UseBasicParsing "$Head/api/v1/join/bundle/windows-$Arch.tar.gz.sha256$Q").Content -split ' ')[0].Trim()
  $have = (Get-FileHash bundle.tgz -Algorithm SHA256).Hash.ToLower()
  if ($want -ne $have) { throw 'checksum mismatch on the downloaded program; not installing' }
  tar -xzf bundle.tgz; $got = $true
  Write-Host 'watch-worker: prebuilt program installed, checksum ok'
} catch { Write-Host "watch-worker: no prebuilt program: $($_.Exception.Message)" }
if (-not $got) {
  if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'This needs Rust to build from source: https://rustup.rs (then run this command again)' }
  Write-Host 'watch-worker: building from the head source (first build takes several minutes)'
  Invoke-WebRequest -UseBasicParsing -OutFile source.tgz "$Head/api/v1/join/source.tar.gz$Q"
  Remove-Item -Recurse -Force build -ErrorAction SilentlyContinue; New-Item -ItemType Directory build | Out-Null
  tar -xzf source.tgz -C build
  Push-Location build; cargo build --release --no-default-features --features watch; Pop-Location
  Copy-Item build\target\release\cw.exe .\cw.exe -Force
}
$args2 = @('join', $HostName, '--port', $Port, '--name', $env:COMPUTERNAME)
if ($Code) { $args2 += @('--code', $Code) }
Write-Host 'watch-worker: starting the worker; press Ctrl-C to leave the cluster'
& .\cw.exe @args2
"##;

// ---------------------------------------------------------------- the joining PC

fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("CW_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join(".watch-worker")
}

fn token_header() -> Option<String> {
    std::env::var("CW_WORKER_TOKEN").ok().filter(|t| !t.is_empty()).map(|t| format!("Bearer {t}"))
}

fn get(base: &str, path: &str) -> Result<ureq::Response, String> {
    let mut r = ureq::get(&format!("{base}{path}")).timeout(Duration::from_secs(600));
    if let Some(t) = token_header() {
        r = r.set("Authorization", &t);
    }
    r.call().map_err(|e| match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or(body);
            format!("{path}: HTTP {code} {msg}")
        }
        e => format!("{path}: {e}"),
    })
}

/// Download one manifest file into `root` unless the right bytes are already there. Verifies sha256 before the rename.
fn ensure_file(base: &str, root: &Path, f: &serde_json::Value) -> Result<bool, String> {
    let rel = f["path"].as_str().ok_or("manifest entry without a path")?;
    let want = f["sha256"].as_str().unwrap_or("");
    let size = f["size"].as_u64().unwrap_or(0);
    if rel.starts_with('/') || rel.split('/').any(|s| s == "..") {
        return Err(format!("refusing path {rel}"));
    }
    let dest = root.join(rel);
    if dest.is_file() && sha256_file(&dest).is_ok_and(|h| h == want) {
        return Ok(false);
    }
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let part = dest.with_extension("part");
    let resp = get(base, &format!("/api/v1/join/file?path={}", rel.replace(' ', "%20")))?;
    let mut rd = resp.into_reader();
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let (mut h, mut got, mut mark) = (Sha256::new(), 0u64, 0u64);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = rd.read(&mut buf).map_err(|e| format!("download of {rel} broke: {e}"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        if size > 0 && got * 10 / size > mark {
            mark = got * 10 / size;
            eprintln!("cw  {rel}: {}%", mark * 10);
        }
    }
    let have: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    drop(out);
    if have != want {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{rel}: checksum mismatch (got {have}, the head says {want}); not installed"));
    }
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    Ok(true)
}

/// A model the head lists but this PC lacks (a module switched on after joining). Used by the worker.
pub fn fetch_from_head(base: &str, rel: &str, root: &Path) -> Result<(), String> {
    let m: serde_json::Value = get(base, "/api/v1/join/manifest")?.into_json().map_err(|e| e.to_string())?;
    let f = m["files"].as_array().and_then(|a| a.iter().find(|f| f["path"].as_str() == Some(rel))).ok_or_else(|| format!("{rel} is not offered by the head"))?;
    log(&format!("fetching {rel} from the head"));
    ensure_file(base, root, f).map(|_| ())
}

fn split_head(arg: &str, default_port: u16) -> (String, u16) {
    let a = arg.trim_start_matches("http://").trim_end_matches('/');
    match a.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.contains(']') && !p.is_empty() => (h.to_string(), p.parse().unwrap_or(default_port)),
        _ => (a.to_string(), default_port),
    }
}

/// `cw join [head-ip[:port]] [--code CODE] [--name N]`
pub fn run_join(opts: Opts) -> Result<(), String> {
    let step = |n: u32, m: &str| eprintln!("cw  [{n}/5] {m}");
    let mut port = opts.port;
    let host = match opts.positional.first().cloned().or(opts.head.clone()) {
        Some(h) => {
            let (h, p) = split_head(&h, port);
            port = p;
            h
        }
        None => {
            step(1, "looking for a head on this network (UDP 7711, up to 30 s)");
            let (ip, p, name) = crate::watch::net::discover_any(Duration::from_secs(30)).ok_or("no head announced itself in 30 s. Give its address: cw join 192.168.1.20 (is UDP 7711 allowed by the firewall?)")?;
            eprintln!("cw       found head '{name}' at {ip}:{p}");
            port = p;
            ip
        }
    };
    let base = format!("http://{host}:{port}");
    step(2, &format!("contacting {base}"));
    ureq::get(&format!("{base}/api/health")).timeout(Duration::from_secs(5)).call().map_err(|e| format!("cannot reach {base}: {e}. Check the address, that the head listens on 0.0.0.0 (CW_BIND), and the firewall (TCP {port})."))?;

    let home = home_dir();
    let root = home.join("root");
    std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let name = opts.node_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(crate::watch::hostname);
    let cache = home.join("join.json");
    let saved: Option<serde_json::Value> = std::fs::read_to_string(&cache).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).filter(|v| v["head"] == base.as_str());
    let token = match (opts.code.as_deref(), saved.as_ref().and_then(|v| v["token"].as_str())) {
        (None, Some(t)) => {
            step(3, "already enrolled with this head (token cached); no code needed");
            t.to_string()
        }
        (code, _) => {
            step(3, "enrolling");
            let body = serde_json::json!({ "code": code.unwrap_or(""), "name": name, "host": crate::watch::hostname(), "os": std::env::consts::OS, "arch": std::env::consts::ARCH });
            let resp = ureq::post(&format!("{base}/api/v1/join/enroll")).timeout(Duration::from_secs(10)).send_json(body).map_err(|e| match e {
                ureq::Error::Status(c, r) => {
                    let m = r.into_json::<serde_json::Value>().ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or_default();
                    format!("the head refused to enroll this PC (HTTP {c}): {m}")
                }
                e => format!("enroll failed: {e}"),
            })?;
            let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
            let t = v["worker_token"].as_str().ok_or("the head did not return a worker token")?.to_string();
            let _ = std::fs::write(&cache, serde_json::json!({ "head": base, "token": t, "name": name }).to_string());
            t
        }
    };
    std::env::set_var("CW_WORKER_TOKEN", &token);

    step(4, "fetching models and settings from the head");
    let m: serde_json::Value = get(&base, "/api/v1/join/manifest")?.into_json().map_err(|e| e.to_string())?;
    for f in m["files"].as_array().into_iter().flatten() {
        match ensure_file(&base, &root, f) {
            Ok(true) => eprintln!("cw       {} downloaded, checksum ok", f["path"].as_str().unwrap_or("")),
            Ok(false) => eprintln!("cw       {} already here, checksum ok", f["path"].as_str().unwrap_or("")),
            Err(e) => return Err(e),
        }
    }
    let vision = root.join("vision");
    let _ = std::fs::create_dir_all(&vision);
    let _ = std::fs::write(vision.join("modules.json"), serde_json::to_string_pretty(&m["modules"]).unwrap_or_default());
    let _ = std::fs::write(vision.join("rules.json"), serde_json::to_string_pretty(&m["rules"]).unwrap_or_default());
    if std::process::Command::new("ffmpeg").arg("-version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_err() {
        eprintln!("cw       warning: ffmpeg is not installed or not on PATH; the worker cannot decode video without it");
    }

    step(5, &format!("running as worker '{name}'. Recordings are read from the head; nothing else is stored here. Ctrl-C leaves the cluster."));
    std::env::set_var("CW_ROOT", &root);
    std::env::set_var("CW_NODE_NAME", &name);
    std::env::set_var("CW_FETCH_MODELS", "1");
    let mut w = Opts { name: opts.name.clone(), head: Some(host), port, no_local: false, local: false, positional: Vec::new(), code: None, node_name: Some(name), dry_run: false };
    w.local = false;
    crate::watch::worker::run(w)
}

/// `cw deploy user@host [--head IP] [--code CODE] [--dry-run]`: ssh to a PC the operator names and run the installer there.
pub fn run_deploy(opts: Opts) -> Result<(), String> {
    let target = opts.positional.first().cloned().ok_or("usage: cw deploy user@host [--head IP] [--code CODE] [--dry-run]")?;
    if target.starts_with('-') || !target.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-' | '_' | ':' | '[' | ']')) {
        return Err(format!("'{target}' does not look like user@host"));
    }
    let head_ip = opts.head.clone().unwrap_or_else(crate::watch::primary_ip);
    let base = format!("http://{head_ip}:{}", opts.port);
    let code = match opts.code.clone() {
        Some(c) => c,
        None if opts.dry_run => "XXXX-XXXX".into(),
        None => {
            let r = ureq::post(&format!("{base}/api/v1/join/codes")).timeout(Duration::from_secs(5)).send_json(serde_json::json!({ "uses": 1 }));
            match r {
                Ok(r) => r.into_json::<serde_json::Value>().ok().and_then(|v| v["code"].as_str().map(str::to_string)).ok_or("the head returned no code")?,
                Err(ureq::Error::Status(c, _)) if c == 401 || c == 403 => {
                    return Err(format!("the head needs a signed-in administrator to make a code. Open {base}/join, create one, and run: cw deploy {target} --head {head_ip} --code CODE"))
                }
                Err(e) => return Err(format!("cannot reach the head at {base}: {e}")),
            }
        }
    };
    let remote = format!("mkdir -p ~/.watch-worker && curl -fsSL '{base}/api/v1/join/install.sh?code={code}' | CW_DETACH=1 sh");
    eprintln!("cw  deploy: the head is {base}; the PC {target} must be able to reach it (TCP {}).", opts.port);
    eprintln!("cw  deploy: this runs on {target} over ssh (you supply the ssh access):");
    eprintln!("    ssh {target} \"{remote}\"");
    if opts.dry_run {
        eprintln!("cw  deploy: dry run, nothing was executed.");
        return Ok(());
    }
    let st = Command::new("ssh").args(["-o", "ConnectTimeout=10", &target, &remote]).status().map_err(|e| format!("could not run ssh: {e}. Install an OpenSSH client."))?;
    if !st.success() {
        return Err(format!("ssh to {target} ended with {st}. Check that you can `ssh {target}` by hand, that curl and tar exist there, and the install lines above."));
    }
    eprintln!("cw  deploy: waiting up to 60 s for the PC to appear on the head");
    for _ in 0..30 {
        std::thread::sleep(Duration::from_secs(2));
        if let Ok(r) = ureq::get(&format!("{base}/api/v1/cluster")).timeout(Duration::from_secs(3)).call() {
            let v: serde_json::Value = r.into_json().unwrap_or_default();
            let host = target.rsplit('@').next().unwrap_or(&target);
            if let Some(n) = v["nodes"].as_array().and_then(|a| a.iter().find(|n| n["ip"] == host || n["host"].as_str().is_some_and(|h| h.starts_with(host) || host.starts_with(h)))) {
                eprintln!("cw  deploy: {} joined ({} {}/{})", n["host"].as_str().unwrap_or(""), n["id"].as_str().unwrap_or(""), n["os"].as_str().unwrap_or(""), n["arch"].as_str().unwrap_or(""));
                return Ok(());
            }
        }
    }
    eprintln!("cw  deploy: the installer ran but no new PC showed up yet; read ~/.watch-worker/worker.log on {target}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_works_once_then_dies() {
        let mut s = State::default();
        let c = s.create(1, 1000.0).unwrap();
        assert!(c.code.len() == 9 && c.code.contains('-'));
        assert!(s.check(&c.code.to_lowercase(), false, 1001.0).is_ok(), "peek does not spend it, case and dash are ignored");
        assert!(s.check(&c.code.replace('-', ""), true, 1002.0).is_ok());
        assert_eq!(s.check(&c.code, true, 1003.0).unwrap_err().code, "unauthorized", "single use");
    }

    #[test]
    fn a_code_expires_after_ten_minutes() {
        let mut s = State::default();
        let c = s.create(3, 0.0).unwrap();
        assert!(s.check(&c.code, true, CODE_SECS - 1.0).is_ok());
        assert!(s.check(&c.code, true, CODE_SECS + 1.0).is_err());
        assert!(s.check("", true, 1.0).is_err(), "an empty code never matches");
    }

    #[test]
    fn guessing_is_rate_limited() {
        let mut s = State::default();
        let c = s.create(1, 0.0).unwrap();
        for _ in 0..10 {
            assert_eq!(s.check("WRONG-000", true, 5.0).unwrap_err().code, "unauthorized");
        }
        assert_eq!(s.check(&c.code, true, 6.0).unwrap_err().code, "rate_limited", "even the right code waits");
        assert!(s.check(&c.code, true, 70.0).is_ok(), "a minute later it works");
    }

    #[test]
    fn host_header_cannot_inject_into_the_script() {
        let h = |v: &str| vec![("Host".to_string(), v.to_string())];
        assert_eq!(head_addr(&h("192.168.1.20:7710")), "192.168.1.20:7710");
        assert!(!head_addr(&h("x'; rm -rf ~; '")).contains('\''));
        assert!(!head_addr(&h("a b")).contains(' '));
    }

    #[test]
    fn head_argument_takes_an_optional_port() {
        assert_eq!(split_head("10.0.0.5", 7710), ("10.0.0.5".into(), 7710));
        assert_eq!(split_head("http://10.0.0.5:7820/", 7710), ("10.0.0.5".into(), 7820));
    }

    #[test]
    fn enrollment_needs_a_code_only_when_auth_is_enforced() {
        let api = crate::watch::api::test_api();
        // The test Auth has no users and CW_AUTH is not "required": open mode, no code needed.
        assert_eq!(api.auth.mode(), "open-trusted-lan");
        let r = enroll(&api, br#"{"name":"lab pc 2","host":"h","os":"linux","arch":"x86_64"}"#).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert!(v["worker_token"].as_str().unwrap().starts_with("cwt_"));
        assert_eq!(v["name"], "lab-pc-2");
    }

    #[test]
    fn with_sign_in_on_a_pc_needs_a_live_single_use_code_for_everything() {
        let dir = std::env::temp_dir().join(format!("cw-join-enforced-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let auth = crate::watch::auth::Auth::load_with_iters(&dir, 1000);
        let t = auth.bootstrap_token().unwrap();
        auth.bootstrap("admin", "correct-horse-battery-staple", &t).unwrap();
        let api = Api::new(crate::watch::storage::memory(), dir, auth);
        assert_eq!(api.auth.mode(), "enforced");
        let status = |m: &str, u: &str, b: &[u8]| handle(&api, m, u, b, &[]).unwrap().status;
        assert_eq!(status("POST", "/api/v1/join/enroll", br#"{"name":"pc"}"#), 401, "no code, no enrollment");
        assert_eq!(status("GET", "/api/v1/join/install.sh", b""), 401, "not even the script without a code");
        assert_eq!(status("GET", "/api/v1/join/source.tar.gz", b""), 401);
        let code = api.join.lock().unwrap().create(1, now_secs()).unwrap().code;
        assert_eq!(status("GET", &format!("/api/v1/join/install.sh?code={code}"), b""), 200);
        let body = format!(r#"{{"code":"{code}","name":"pc","host":"h","os":"linux","arch":"x86_64"}}"#);
        assert_eq!(status("POST", "/api/v1/join/enroll", body.as_bytes()), 200);
        assert_eq!(status("POST", "/api/v1/join/enroll", body.as_bytes()), 401, "the code was single use");
        assert_eq!(status("GET", "/api/v1/join/manifest", b""), 200, "manifest is only reachable past the auth layer (worker token), not by this handler");
    }

    #[test]
    fn scripts_have_the_head_filled_in_and_no_leftover_placeholders() {
        let api = crate::watch::api::test_api();
        let h = vec![("Host".to_string(), "10.1.2.3:7710".to_string())];
        let r = handle(&api, "GET", "/api/v1/join/install.sh?code=ABCD-2345", &[], &h).unwrap();
        let t = String::from_utf8(r.body).unwrap();
        assert!(t.contains("HEAD=\"http://10.1.2.3:7710\"") && t.contains("CODE=\"ABCD-2345\"") && !t.contains("__"));
        let r = handle(&api, "GET", "/api/v1/join/install.ps1", &[], &h).unwrap();
        let t = String::from_utf8(r.body).unwrap();
        assert!(t.contains("'http://10.1.2.3:7710'") && !t.contains("__"));
        let bad = handle(&api, "GET", "/api/v1/join/install.sh?code=x';y", &[], &h).unwrap();
        assert_eq!(bad.status, 400);
    }

    #[test]
    fn a_bundle_for_another_os_says_build_from_source() {
        let api = crate::watch::api::test_api();
        let r = handle(&api, "GET", "/api/v1/join/bundle/plan9-mips.tar.gz", &[], &[]).unwrap();
        assert_eq!(r.status, 404);
        assert!(String::from_utf8_lossy(&r.body).contains("Build from source"));
    }
}
