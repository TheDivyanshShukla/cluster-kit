//! `/api/v1`: versioned routes over the `Store` trait. head.rs owns the socket and hands
//! requests here; anything this does not recognise falls through to the legacy routes.
//! Every failure uses the `{"error": ApiError}` envelope.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::watch::alerts;
use crate::watch::config::Camera;
use crate::watch::domain::*;
use crate::watch::query::v1 as q;
use crate::watch::scheduler::{Scheduler, TaskSpec, WorkerCaps};

pub struct Reply {
    pub status: u16,
    pub ctype: &'static str,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl Reply {
    pub fn json<T: serde::Serialize>(v: &T) -> Reply {
        Reply { status: 200, ctype: "application/json", body: serde_json::to_vec(v).unwrap_or_default(), headers: Vec::new() }
    }
    pub fn err(e: ApiError) -> Reply {
        let status = e.status();
        Reply { status, ctype: "application/json", body: serde_json::to_vec(&serde_json::json!({ "error": e })).unwrap_or_default(), headers: Vec::new() }
    }
    fn bytes(body: Vec<u8>, ctype: &'static str) -> Reply {
        Reply { status: 200, ctype, body, headers: Vec::new() }
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new("invalid_request", msg)
}
fn missing(what: &str) -> ApiError {
    ApiError::new("not_found", format!("{what} not found"))
}

/// Server-sent events. A bounded ring; clients resume with `Last-Event-ID` or `?cursor=`.
pub struct Bus {
    ring: Mutex<(u64, VecDeque<(u64, String, String)>)>,
    wake: Condvar,
}

impl Bus {
    pub fn new() -> Self {
        Bus { ring: Mutex::new((0, VecDeque::new())), wake: Condvar::new() }
    }
    pub fn publish(&self, kind: &str, data: &serde_json::Value) {
        let mut g = self.ring.lock().unwrap();
        g.0 += 1;
        let id = g.0;
        g.1.push_back((id, kind.to_string(), data.to_string()));
        // ponytail: 1000-event ring; a client away longer gets a `reset` and refetches.
        while g.1.len() > 1000 {
            g.1.pop_front();
        }
        self.wake.notify_all();
    }
    fn after(&self, cursor: u64, wait: Duration) -> Vec<(u64, String, String)> {
        let mut g = self.ring.lock().unwrap();
        if g.0 <= cursor {
            g = self.wake.wait_timeout(g, wait).unwrap().0;
        }
        g.1.iter().filter(|e| e.0 > cursor).cloned().collect()
    }
    fn oldest(&self) -> u64 {
        self.ring.lock().unwrap().1.front().map(|e| e.0).unwrap_or(0)
    }
}

/// Streaming SSE body for tiny_http.
pub struct SseBody {
    pub api: Arc<Api>,
    pub cursor: u64,
    pending: Vec<u8>,
    pos: usize,
    started: bool,
}

impl SseBody {
    pub fn new(api: Arc<Api>, cursor: u64) -> Self {
        SseBody { api, cursor, pending: Vec::new(), pos: 0, started: false }
    }
}

impl Read for SseBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.pending.len() {
            let mut out = String::new();
            if !self.started {
                self.started = true;
                out.push_str("retry: 3000\n\n");
                if self.cursor > 0 && self.cursor + 1 < self.api.bus.oldest() {
                    out.push_str("event: reset\ndata: {}\n\n");
                }
            }
            for (id, kind, data) in self.api.bus.after(self.cursor, Duration::from_secs(15)) {
                out.push_str(&format!("id: {id}\nevent: {kind}\ndata: {data}\n\n"));
                self.cursor = id;
            }
            if out.is_empty() {
                out.push_str(": keepalive\n\n");
            }
            self.pending = out.into_bytes();
            self.pos = 0;
        }
        let n = (self.pending.len() - self.pos).min(buf.len());
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct Export {
    id: String,
    review_id: String,
    status: ExportStatus,
    progress: f32,
    error: Option<String>,
    output: Option<String>,
    child: Arc<Mutex<Option<Child>>>,
}

pub struct Api {
    pub store: Arc<dyn Store>,
    pub bus: Bus,
    pub data_dir: PathBuf,
    exports: Mutex<HashMap<String, Export>>,
    pub sched: Mutex<Scheduler>,
    pub engine: Mutex<alerts::Engine>,
    /// Leases handed out, by task ID. The scheduler stays the authority; this keeps the full lease body.
    held: Mutex<HashMap<String, TaskLease>>,
    /// camera id -> (url|kind key, task id). A camera is re-registered only when its location changes.
    catalog: Mutex<HashMap<String, (String, String)>>,
    /// Last analyzed frame per source with its own detections, for an in-sync live overlay.
    latest: Mutex<HashMap<String, serde_json::Value>>,
    /// Rule diagnostics already written to the audit log: (rule, version, source, code). Each is logged once.
    /// Result of validating the configured models, valid while the module list and model files are unchanged.
    model_cache: Mutex<Option<(String, Vec<(String, Result<ModelVersion, ApiError>)>)>>,
    diag_seen: Mutex<std::collections::HashSet<(String, String, String, String)>>,
    pub auth: crate::watch::auth::Auth,
}

/// Lease time. A worker polls `/api/work` every ~2 s and heartbeats, so 15 s means ~7 missed polls.
const LEASE_US: Us = 15_000_000;

impl Api {
    pub fn new(store: Arc<dyn Store>, data_dir: PathBuf, auth: crate::watch::auth::Auth) -> Self {
        let specs = crate::watch::config::load_rules().iter().filter(|r| r.enabled).filter_map(|r| alerts::RuleSpec::from_legacy(r).ok()).collect();
        let mut engine = alerts::Engine::new(specs);
        if let Some(snap) = std::fs::read(data_dir.join("alerts_state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            for a in engine.restore(snap) {
                let _ = store.upsert_alert(&a);
            }
        }
        Api {
            store,
            bus: Bus::new(),
            data_dir,
            exports: Mutex::new(HashMap::new()),
            sched: Mutex::new(Scheduler::new(LEASE_US)),
            engine: Mutex::new(engine),
            held: Mutex::new(HashMap::new()),
            catalog: Mutex::new(HashMap::new()),
            latest: Mutex::new(HashMap::new()),
            model_cache: Mutex::new(None),
            diag_seen: Mutex::new(Default::default()),
            auth,
        }
    }

    pub fn save_alert_state(&self) {
        let snap = self.engine.lock().unwrap().snapshot();
        if let Ok(b) = serde_json::to_vec(&snap) {
            let _ = std::fs::write(self.data_dir.join("alerts_state.json"), b);
        }
    }
}

pub fn param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&').filter_map(|p| p.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| decode(v))
}

fn decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Some(n) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(n);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or(s)
}

fn us_param(url: &str, key: &str) -> Result<Option<Us>, ApiError> {
    match param(url, key) {
        None => Ok(None),
        Some(v) => v.parse().map(Some).map_err(|_| bad(format!("{key} must be integer microseconds"))),
    }
}

fn page(url: &str, default: u32) -> Page {
    Page { after: param(url, "cursor").filter(|s| !s.is_empty()), limit: param(url, "limit").and_then(|s| s.parse().ok()).unwrap_or(default).clamp(1, 1000) }
}

fn now_us() -> Us {
    (crate::watch::now_secs() * 1e6) as Us
}

fn body_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| bad(format!("invalid JSON: {e}")))
}

/// Routes that need only the store and FFmpeg. Returns None when the path is not ours.
pub fn handle(api: &Arc<Api>, method: &str, url: &str, body: &[u8], range: Option<&str>) -> Option<Reply> {
    let path = url.split('?').next().unwrap_or(url);
    let rest = path.strip_prefix("/api/v1/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let r = match (method, parts.as_slice()) {
        ("POST", ["index"]) => index(api, body),
        ("POST", ["heartbeat"]) => heartbeat(api, body),
        ("POST", ["lease"]) => lease(api, body),
        ("GET", ["cluster"]) => {
            let s = api.sched.lock().unwrap();
            Ok(Reply::json(&serde_json::json!({ "workers": s.workers(), "tasks": s.tasks() })))
        }
        ("POST", ["queries"]) => run_query(api, body),
        ("GET", ["queries", id]) => q::get_run(&*api.store, id).map(|r| Reply::json(&serde_json::json!({ "run": r }))),
        ("GET", ["queries", id, "matches"]) => {
            let p = page(url, 200);
            q::matches_page(&*api.store, id, p.after, p.limit).map(|(m, next)| Reply::json(&serde_json::json!({ "matches": m, "next": next })))
        }
        ("GET", ["queries", id, "timeline"]) => (|| {
            let start = us_param(url, "start_us")?.unwrap_or(0);
            let end = us_param(url, "end_us")?.unwrap_or(i64::MAX / 4);
            let res = us_param(url, "resolution_us")?.unwrap_or(0);
            q::timeline(&*api.store, id, start, end, res).map(|v| Reply::json(&tidy_timeline(api, v)))
        })(),
        ("GET", ["models"]) => models(api),
        // No analyzed frame yet is a normal state for a camera that just started: 204, not an error.
        ("GET", ["live", id, "latest"]) => Ok(api.latest.lock().unwrap().get(*id).cloned().map(|v| Reply::json(&v)).unwrap_or(Reply { status: 204, ctype: "application/json", body: Vec::new(), headers: Vec::new() })),
        ("GET", ["observations", id, "thumbnail"]) => {
            std::fs::read(api.data_dir.join("thumbs").join(format!("{}.jpg", sanitize(id)))).map(|b| Reply::bytes(b, "image/jpeg")).map_err(|_| missing("thumbnail"))
        }
        ("GET", ["alerts"]) => api.store.alerts(&page(url, 100)).map(|(a, next)| Reply::json(&serde_json::json!({ "alerts": a, "next": next }))),
        ("POST", ["alerts", id, "decision"]) => decide(api, id, body),
        ("POST", ["rules", "backtest"]) => backtest(api, body),
        ("POST", ["sources"]) => save_source(api, body),
        ("GET", ["sources"]) => api.store.sources().map(|v| Reply::json(&v)),
        ("GET", ["jobs"]) => api.store.jobs().map(|v| Reply::json(&v)),
        ("GET", ["jobs", id]) => api.store.job(id).and_then(|j| j.ok_or_else(|| missing("job"))).map(|j| Reply::json(&j)),
        ("GET", ["media", id]) => media(api, id).map(|m| Reply::json(&public_media(m))),
        ("GET", ["media", id, "stream"]) => stream(api, id, range),
        ("GET", ["media", id, "frame"]) => frame(api, id, url),
        ("GET", ["media", id, "thumbnails"]) => thumbnails(api, id, url),
        ("GET", ["media", id, "sprite", name]) => sprite_file(api, id, name),
        ("GET", ["media", id, "observations"]) => observations(api, id, url),
        ("GET", ["reviews"]) => api.store.reviews().map(|v| Reply::json(&v)),
        ("POST", ["reviews"]) => create_review(api, body),
        ("GET", ["reviews", id]) => api.store.review(id).and_then(|r| r.ok_or_else(|| missing("review"))).map(|r| Reply::json(&r)),
        ("PATCH", ["reviews", id]) => patch_review(api, id, body),
        ("POST", ["reviews", id, "manifest"]) | ("GET", ["reviews", id, "manifest"]) => manifest(api, id),
        ("POST", ["exports"]) => start_export(api, body),
        ("GET", ["exports", id]) => export_status(api, id),
        ("GET", ["exports", id, "file"]) => export_file(api, id),
        ("POST", ["exports", id, "cancel"]) => cancel_export(api, id),
        ("GET", ["audit"]) => {
            let q = param(url, "q").filter(|s| !s.is_empty());
            api.store.audit_log(q.as_deref(), &page(url, 100)).map(|(items, next)| Reply::json(&serde_json::json!({ "items": items, "next": next })))
        }
        _ => return None,
    };
    Some(r.unwrap_or_else(Reply::err))
}

fn index(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let b: IndexBatch = body_json(body)?;
    validate_batch(&b)?;
    let thumb = match &b.thumbnail_hex {
        Some(hex) if !hex.is_empty() => Some(save_thumbnail(api, &b.observation.observation_id, hex)?),
        _ => None,
    };
    let mut b = b;
    if let Some(name) = thumb {
        b.observation.thumbnail = Some(name);
    }
    b.thumbnail_hex = None;
    if let Some(l) = &b.lease {
        let mut s = api.sched.lock().unwrap();
        s.reap(now_us());
        s.check(l)?;
    }
    let out = api.store.commit_batch(&b)?;
    if let Some(t) = &b.observation.thumbnail {
        let o = &b.observation;
        let newer = api.latest.lock().unwrap().get(&o.source_id).and_then(|v| v["media_time_us"].as_i64()).map_or(true, |t0| o.media_time_us >= t0 || o.media_time_us + 5_000_000 < t0);
        if newer {
            let dets: Vec<_> = b.detections.iter().map(|d| serde_json::json!({ "class": d.class, "confidence": d.confidence, "bbox": d.bbox, "track_id": d.track_id, "color": d.color })).collect();
            api.latest.lock().unwrap().insert(o.source_id.clone(), serde_json::json!({
                "observation_id": o.observation_id, "media_id": o.media_id, "media_time_us": o.media_time_us,
                "frame": format!("/api/v1/observations/{}/thumbnail", sanitize(&o.observation_id)), "detections": dets, "file": t,
            }));
        }
    }
    if !out.duplicate {
        evaluate(api, &b.observation);
        api.bus.publish("index", &serde_json::json!({ "media_id": b.observation.media_id, "media_time_us": b.observation.media_time_us, "detections": b.detections.len(), "snapshot": out.snapshot }));
    }
    Ok(Reply::json(&out))
}

pub fn validate_batch(b: &IndexBatch) -> Result<(), ApiError> {
    if b.schema_version != SCHEMA_VERSION {
        return Err(bad(format!("schema_version {} unsupported; expected {SCHEMA_VERSION}", b.schema_version)));
    }
    let o = &b.observation;
    if o.observation_id.is_empty() || o.media_id.is_empty() || o.source_id.is_empty() {
        return Err(bad("observation_id, media_id and source_id are required"));
    }
    if o.media_time_us < 0 {
        return Err(bad("media_time_us must be nonnegative"));
    }
    if b.detections.len() > 10_000 {
        return Err(ApiError::new("too_large", "batch exceeds 10000 detections"));
    }
    for d in &b.detections {
        if d.observation_id != o.observation_id {
            return Err(bad("detection observation_id does not match the batch"));
        }
        if d.class.is_empty() || !(0.0..=1.0).contains(&d.confidence) || d.bbox.iter().any(|n| !n.is_finite()) {
            return Err(bad(format!("detection {} needs a class, confidence 0..1 and a finite box", d.detection_id)));
        }
    }
    for r in &b.relations {
        if r.observation_id != o.observation_id {
            return Err(bad("relation observation_id does not match the batch"));
        }
    }
    Ok(())
}

fn save_thumbnail(api: &Api, obs: &str, hex: &str) -> Result<String, ApiError> {
    if hex.len() > 600_000 || hex.len() % 2 != 0 {
        return Err(ApiError::new("too_large", "thumbnail exceeds 300KB or has odd hex length"));
    }
    let jpeg: Vec<u8> = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|p| std::str::from_utf8(p).ok().and_then(|t| u8::from_str_radix(t, 16).ok()))
        .collect::<Option<_>>()
        .ok_or_else(|| bad("invalid thumbnail hex"))?;
    if !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9]) {
        return Err(bad("thumbnail must be JPEG"));
    }
    let dir = api.data_dir.join("thumbs");
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::new("internal", e.to_string()))?;
    // Name by observation ID: a retry rewrites the same file instead of adding one.
    let name = format!("{obs}.jpg");
    std::fs::write(dir.join(&name), &jpeg).map_err(|e| ApiError::new("internal", e.to_string()))?;
    Ok(name)
}

fn media(api: &Api, id: &str) -> Result<MediaAsset, ApiError> {
    api.store.media(id)?.ok_or_else(|| missing("media"))
}

fn media_file(api: &Api, id: &str) -> Result<(MediaAsset, PathBuf), ApiError> {
    let m = media(api, id)?;
    let p = PathBuf::from(crate::watch::config::resolve_url(&m.uri));
    if !p.is_file() {
        return Err(ApiError::new("unavailable", format!("original media for {id} is not available on the head")));
    }
    Ok((m, p))
}

fn stream(api: &Api, id: &str, range: Option<&str>) -> Result<Reply, ApiError> {
    let (m, path) = media_file(api, id)?;
    let ctype = if m.still { image_type(&path) } else { "video/mp4" };
    let mut f = std::fs::File::open(&path).map_err(|e| ApiError::new("unavailable", e.to_string()))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len == 0 {
        return Err(ApiError::new("unavailable", "media file is empty"));
    }
    let (start, end, status) = match range {
        Some(h) => match crate::watch::head::parse_range(h, len) {
            Some((s, e)) => (s, e, 206),
            None => {
                let mut r = Reply::err(ApiError::new("invalid_request", "unsatisfiable range"));
                r.status = 416;
                r.headers.push(("Content-Range".into(), format!("bytes */{len}")));
                return Ok(r);
            }
        },
        None if len > 16 * 1024 * 1024 => (0, 16 * 1024 * 1024 - 1, 206),
        None => (0, len - 1, 200),
    };
    use std::io::{Seek, SeekFrom};
    let mut buf = vec![0u8; (end - start + 1) as usize];
    f.seek(SeekFrom::Start(start)).and_then(|_| f.read_exact(&mut buf)).map_err(|e| ApiError::new("unavailable", e.to_string()))?;
    let mut r = Reply::bytes(buf, ctype);
    r.status = status;
    r.headers.push(("Accept-Ranges".into(), "bytes".into()));
    if status == 206 {
        r.headers.push(("Content-Range".into(), format!("bytes {start}-{end}/{len}")));
    }
    Ok(r)
}

fn image_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        _ => "image/jpeg",
    }
}

/// Run FFmpeg to completion and return stdout. `output()` always reaps the child.
fn ffmpeg(args: &[String]) -> Result<Vec<u8>, ApiError> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ApiError::new("unavailable", format!("ffmpeg did not start: {e}")))?;
    if !out.status.success() || out.stdout.is_empty() {
        let msg = String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("no frame decoded").to_string();
        return Err(ApiError::new("unavailable", format!("ffmpeg: {msg}")));
    }
    Ok(out.stdout)
}

fn secs(us: Us) -> String {
    format!("{:.6}", us as f64 / 1e6)
}

/// One JPEG at a media time. Input-side seek with re-encode is frame-accurate in FFmpeg >= 2.1.
fn frame(api: &Api, id: &str, url: &str) -> Result<Reply, ApiError> {
    // Thumbnails of media with no original on this head (legacy imports, deleted files) are simply absent:
    // 204 lets the page show its placeholder without a failing request.
    let (m, path) = match media_file(api, id) {
        Ok(v) => v,
        Err(e) if e.code == "unavailable" => return Ok(Reply { status: 204, ctype: "image/jpeg", body: Vec::new(), headers: Vec::new() }),
        Err(e) => return Err(e),
    };
    let t = us_param(url, "t_us")?.unwrap_or(0).max(0);
    let width: u32 = param(url, "w").and_then(|s| s.parse().ok()).unwrap_or(320).clamp(32, 1920);
    let mut args = Vec::new();
    if !m.still {
        args.extend(["-ss".into(), secs(t)]);
    }
    args.extend(["-i".into(), path.to_string_lossy().into_owned(), "-frames:v".into(), "1".into(), "-vf".into(), format!("scale={width}:-2"), "-f".into(), "image2pipe".into(), "-vcodec".into(), "mjpeg".into(), "-".into()]);
    let mut r = Reply::bytes(ffmpeg(&args)?, "image/jpeg");
    r.headers.push(("Cache-Control".into(), "max-age=3600".into()));
    Ok(r)
}

/// Contact sheet: one sprite of evenly spaced samples. Sample times are the requested times;
/// FFmpeg's fps filter picks the nearest decoded frame, so they are within one frame of it.
fn thumbnails(api: &Api, id: &str, url: &str) -> Result<Reply, ApiError> {
    let (m, path) = media_file(api, id)?;
    let start = us_param(url, "start_us")?.unwrap_or(0).max(0);
    let dur = m.duration_us.unwrap_or(0);
    let end = us_param(url, "end_us")?.unwrap_or(dur).max(start + 1);
    let end = if dur > 0 { end.min(dur) } else { end };
    let step = us_param(url, "step_us")?.unwrap_or(1_000_000).max(40_000);
    // Samples at start, start+step, ... strictly before `end`: a sample at the end has no frame.
    let n = (((end - start - 1) / step) + 1).clamp(1, 100);
    let step = if n == 100 { (end - start) / 100 } else { step }.max(1);
    let cols = n.min(10);
    let rows = (n + cols - 1) / cols;
    let (tw, th) = (160u32, 90u32);
    let name = format!("{}-{}-{}.jpg", start, end, step);
    let dir = api.data_dir.join("sprites").join(sanitize(id));
    let file = dir.join(&name);
    if !file.is_file() {
        let args: Vec<String> = vec![
            "-ss".into(), secs(start), "-t".into(), secs(end - start), "-i".into(), path.to_string_lossy().into_owned(),
            "-vf".into(), format!("fps=1/{},scale={tw}:{th}:force_original_aspect_ratio=decrease,pad={tw}:{th}:(ow-iw)/2:(oh-ih)/2,tile={cols}x{rows}", secs(step)),
            "-frames:v".into(), "1".into(), "-f".into(), "image2pipe".into(), "-vcodec".into(), "mjpeg".into(), "-".into(),
        ];
        let jpeg = ffmpeg(&args)?;
        std::fs::create_dir_all(&dir).map_err(|e| ApiError::new("internal", e.to_string()))?;
        std::fs::write(&file, jpeg).map_err(|e| ApiError::new("internal", e.to_string()))?;
    }
    let samples: Vec<_> = (0..n).map(|i| serde_json::json!({ "t_us": start + i * step, "x": (i % cols) as u32 * tw, "y": (i / cols) as u32 * th })).collect();
    Ok(Reply::json(&serde_json::json!({
        "sprite": format!("/api/v1/media/{id}/sprite/{name}"),
        "tile_w": tw, "tile_h": th, "cols": cols, "rows": rows,
        "samples": samples,
        "time_precision": "nearest decoded frame to each requested time",
    })))
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

fn sprite_file(api: &Api, id: &str, name: &str) -> Result<Reply, ApiError> {
    if !name.ends_with(".jpg") || !name.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '.' || c == 'j' || c == 'p' || c == 'g') {
        return Err(bad("bad sprite name"));
    }
    let data = std::fs::read(api.data_dir.join("sprites").join(sanitize(id)).join(name)).map_err(|_| missing("sprite"))?;
    let mut r = Reply::bytes(data, "image/jpeg");
    r.headers.push(("Cache-Control".into(), "max-age=86400".into()));
    Ok(r)
}

fn observations(api: &Api, id: &str, url: &str) -> Result<Reply, ApiError> {
    let from = us_param(url, "start_us")?.unwrap_or(0);
    let to = us_param(url, "end_us")?.unwrap_or(from + 10_000_000);
    if to < from || to - from > 600_000_000 {
        return Err(bad("observation window must be 0..600 s"));
    }
    let rows = api.store.observations(id, from, to)?;
    let out: Vec<_> = rows.into_iter().map(|(o, d, r)| serde_json::json!({ "observation": o, "detections": d, "relations": r })).collect();
    Ok(Reply::json(&out))
}

#[derive(serde::Deserialize)]
struct NewReview {
    title: String,
    #[serde(default)]
    items: Vec<SelectionItem>,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    actor: Option<String>,
}

fn check_items(api: &Api, items: &[SelectionItem]) -> Result<(), ApiError> {
    for it in items {
        if it.out_us < it.in_us {
            return Err(bad(format!("selection {} ends before it starts", it.match_id)));
        }
        // Trim stays inside the media; original evidence is never changed.
        if let Some(m) = api.store.review_match(&it.match_id)? {
            if let Some(media) = api.store.media(&m.media_id)? {
                if it.in_us < 0 || media.duration_us.map(|d| it.out_us > d).unwrap_or(false) {
                    return Err(bad(format!("selection {} is outside its media", it.match_id)));
                }
            }
        }
    }
    Ok(())
}

fn create_review(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let n: NewReview = body_json(body)?;
    if n.title.trim().is_empty() {
        return Err(bad("title required"));
    }
    check_items(api, &n.items)?;
    let now = now_us();
    let r = ReviewSelection {
        id: stable_id("rev", &[&n.title, &now.to_string()]),
        version: 0,
        title: n.title,
        items: n.items,
        notes: n.notes,
        actor: n.actor,
        export: ExportStatus::None,
        updated_utc_us: now,
    };
    let saved = api.store.save_review(&r, None)?;
    audit(api, saved.actor.as_deref(), "review.create", &[&saved.id], "ok", &format!("{} clips", saved.items.len()));
    Ok(Reply::json(&saved))
}

#[derive(serde::Deserialize)]
struct PatchReview {
    version: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    items: Option<Vec<SelectionItem>>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    actor: Option<String>,
}

fn patch_review(api: &Api, id: &str, body: &[u8]) -> Result<Reply, ApiError> {
    let p: PatchReview = body_json(body)?;
    let mut r = api.store.review(id)?.ok_or_else(|| missing("review"))?;
    if let Some(t) = p.title {
        r.title = t;
    }
    if let Some(items) = p.items {
        check_items(api, &items)?;
        r.items = items;
    }
    if let Some(n) = p.notes {
        r.notes = n;
    }
    if p.actor.is_some() {
        r.actor = p.actor;
    }
    r.updated_utc_us = now_us();
    let saved = api.store.save_review(&r, Some(p.version))?;
    let decisions: Vec<String> = saved.items.iter().map(|i| format!("{}={:?}", i.match_id, i.verification)).collect();
    audit(api, saved.actor.as_deref(), "review.update", &[&saved.id], "ok", &format!("v{} {}", saved.version, decisions.join(" ")));
    Ok(Reply::json(&saved))
}

/// Evidence manifest: what was reviewed, where it came from, and who decided what.
fn manifest(api: &Api, id: &str) -> Result<Reply, ApiError> {
    let r = api.store.review(id)?.ok_or_else(|| missing("review"))?;
    let models = api.store.models()?;
    let mut clips = Vec::new();
    for it in &r.items {
        let m = api.store.review_match(&it.match_id)?;
        let media = match &m {
            Some(m) => api.store.media(&m.media_id)?,
            None => None,
        };
        let run = match &m {
            Some(m) => api.store.query_run(&m.query_run_id)?,
            None => None,
        };
        clips.push(serde_json::json!({
            "selection": it,
            "match": m,
            "media": media,
            "query": run.map(|q| serde_json::json!({ "id": q.id, "normalized": q.normalized, "snapshot": q.snapshot, "warnings": q.warnings })),
        }));
    }
    let doc = serde_json::json!({
        "manifest_version": 1,
        "generated_utc_us": now_us(),
        "review": r,
        "clips": clips,
        "models": models,
        "notice": "Times are media-relative microseconds unless capture_start_utc_us is set. Confidence is a detector score, not a probability of correctness. Original media is not modified.",
    });
    audit(api, r.actor.as_deref(), "review.manifest", &[&r.id], "ok", "evidence manifest generated");
    let mut out = Reply::json(&doc);
    out.headers.push(("Content-Disposition".into(), format!("attachment; filename=\"{}-manifest.json\"", sanitize(&r.id))));
    Ok(out)
}

pub fn audit(api: &Api, actor: Option<&str>, action: &str, targets: &[&str], result: &str, detail: &str) {
    let now = now_us();
    let rec = AuditRecord {
        id: stable_id("aud", &[action, &now.to_string(), &targets.join(",")]),
        utc_us: now,
        actor: actor.unwrap_or("local").to_string(),
        action: action.into(),
        targets: targets.iter().map(|s| s.to_string()).collect(),
        result: result.into(),
        correlation_id: None,
        detail: detail.into(),
    };
    if let Err(e) = api.store.audit(&rec) {
        crate::watch::log(&format!("audit write failed: {}", e.message));
    }
}

#[derive(serde::Deserialize)]
struct NewExport {
    review_id: String,
}

/// Rendered export: one FFmpeg job concatenating the selection's in/out ranges. Re-encodes
/// (libx264) so cut points are exact to the decoded frame; this is not a stream copy.
fn start_export(api: &Arc<Api>, body: &[u8]) -> Result<Reply, ApiError> {
    let n: NewExport = body_json(body)?;
    let r = api.store.review(&n.review_id)?.ok_or_else(|| missing("review"))?;
    if r.items.is_empty() {
        return Err(bad("review has no clips to export"));
    }
    let mut inputs = Vec::new();
    for it in &r.items {
        let m = api.store.review_match(&it.match_id)?.ok_or_else(|| missing(&format!("match {}", it.match_id)))?;
        let (media, path) = media_file(api, &m.media_id)?;
        if media.still {
            return Err(ApiError::new("unsupported", "image matches are exported through the manifest, not as video"));
        }
        inputs.push((path, it.in_us, it.out_us));
    }
    let id = stable_id("exp", &[&r.id, &r.version.to_string(), &now_us().to_string()]);
    let dir = api.data_dir.join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::new("internal", e.to_string()))?;
    let out = dir.join(format!("{id}.mp4"));
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into(), "-nostdin".into(), "-y".into(), "-progress".into(), "pipe:1".into()];
    for (p, a, b) in &inputs {
        args.extend(["-ss".into(), secs(*a), "-to".into(), secs(*b), "-i".into(), p.to_string_lossy().into_owned()]);
    }
    let total: f64 = inputs.iter().map(|(_, a, b)| (b - a) as f64 / 1e6).sum();
    let streams: String = (0..inputs.len()).map(|i| format!("[{i}:v:0]")).collect();
    args.extend(["-filter_complex".into(), format!("{streams}concat=n={}:v=1:a=0[v]", inputs.len()), "-map".into(), "[v]".into(), "-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-pix_fmt".into(), "yuv420p".into(), out.to_string_lossy().into_owned()]);
    let child = Command::new("ffmpeg").args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| ApiError::new("unavailable", format!("ffmpeg did not start: {e}")))?;
    let child = Arc::new(Mutex::new(Some(child)));
    api.exports.lock().unwrap().insert(id.clone(), Export { id: id.clone(), review_id: r.id.clone(), status: ExportStatus::Running, progress: 0.0, error: None, output: None, child: child.clone() });
    audit(api, r.actor.as_deref(), "export.start", &[&r.id, &id], "queued", &format!("{} clips, {:.1}s", inputs.len(), total));
    let stdout = child.lock().unwrap().as_mut().and_then(|c| c.stdout.take());
    let job = id.clone();
    let partial = out.clone();
    let api_for_reply = api.clone();
    let api = api.clone();
    std::thread::spawn(move || {
        let id = job;
        if let Some(out_pipe) = stdout {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out_pipe).lines().map_while(Result::ok) {
                if let Some(v) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
                    if let Some(e) = api.exports.lock().unwrap().get_mut(&id) {
                        e.progress = if total > 0.0 { (v / 1e6 / total).clamp(0.0, 1.0) as f32 } else { 0.0 };
                    }
                    api.bus.publish("export", &serde_json::json!({ "id": id, "progress": v / 1e6 / total.max(1e-6) }));
                }
            }
        }
        let taken = child.lock().unwrap().take();
        let (ok, err) = match taken.map(|c| c.wait_with_output()) {
            Some(Ok(o)) => (o.status.success(), String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("").to_string()),
            Some(Err(e)) => (false, e.to_string()),
            None => (false, "cancelled".into()),
        };
        let mut g = api.exports.lock().unwrap();
        if let Some(e) = g.get_mut(&id) {
            if e.status == ExportStatus::Cancelled {
                // A cancelled render leaves no half-written file behind.
                let _ = std::fs::remove_file(&partial);
            } else {
                e.status = if ok { ExportStatus::Done } else { ExportStatus::Failed };
                e.progress = if ok { 1.0 } else { e.progress };
                e.output = ok.then(|| format!("/api/v1/exports/{id}/file"));
                e.error = (!ok).then_some(if err.is_empty() { "ffmpeg failed".into() } else { err });
            }
            api.bus.publish("export", &export_json(e));
        }
    });
    export_status(&api_for_reply, &id)
}

fn export_json(e: &Export) -> serde_json::Value {
    serde_json::json!({ "id": e.id, "review_id": e.review_id, "status": e.status, "progress": e.progress, "error": e.error, "output": e.output, "encoding": "libx264 re-encode; cuts at decoded-frame precision" })
}

fn export_status(api: &Api, id: &str) -> Result<Reply, ApiError> {
    api.exports.lock().unwrap().get(id).map(|e| Reply::json(&export_json(e))).ok_or_else(|| missing("export"))
}

fn export_file(api: &Api, id: &str) -> Result<Reply, ApiError> {
    let done = api.exports.lock().unwrap().get(id).map(|e| e.status == ExportStatus::Done).unwrap_or(false);
    if !done {
        return Err(missing("finished export"));
    }
    let data = std::fs::read(api.data_dir.join("exports").join(format!("{}.mp4", sanitize(id)))).map_err(|_| missing("export file"))?;
    let mut r = Reply::bytes(data, "video/mp4");
    r.headers.push(("Content-Disposition".into(), format!("attachment; filename=\"{}.mp4\"", sanitize(id))));
    Ok(r)
}

fn cancel_export(api: &Api, id: &str) -> Result<Reply, ApiError> {
    let mut g = api.exports.lock().unwrap();
    let e = g.get_mut(id).ok_or_else(|| missing("export"))?;
    if e.status == ExportStatus::Running {
        if let Some(c) = e.child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
        e.status = ExportStatus::Cancelled;
        e.error = Some("cancelled by user".into());
    }
    Ok(Reply::json(&export_json(e)))
}

/// Feed the committed frame summary to the alert engine. One inference pass serves every rule.
fn evaluate(api: &Api, o: &FrameObservation) {
    let f = DetectionFilter { media_ids: vec![o.media_id.clone()], media_from_us: Some(o.media_time_us), media_to_us: Some(o.media_time_us), ..Default::default() };
    let Ok((sums, _)) = api.store.summaries(&f, &Page { after: None, limit: 4 }) else { return };
    let mut eng = api.engine.lock().unwrap();
    for s in sums.iter().filter(|s| s.observation_id == o.observation_id) {
        for a in eng.observe(s) {
            let _ = api.store.upsert_alert(&a);
            api.bus.publish("alert", &serde_json::to_value(&a).unwrap_or_default());
        }
    }
    // A diagnostic repeats on every frame (e.g. a weapon rule with no weapon model). Log each distinct
    // one once; "unknown" means the rule cannot be evaluated, which is not a failure.
    for d in eng.take_diagnostics() {
        let key = (d.rule_id.clone(), d.rule_version.clone(), d.source_id.clone(), d.code.clone());
        if !api.diag_seen.lock().unwrap().insert(key) {
            continue;
        }
        let result = if d.level == "error" { "error" } else { "unknown" };
        audit(api, Some("rules"), "rule.diagnostic", &[&d.rule_id], result, &format!("rule {} on {}: {} ({})", d.rule_id, d.source_id, d.message, d.code));
    }
}

#[derive(serde::Deserialize)]
struct Beat {
    caps: WorkerCaps,
}

fn heartbeat(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let b: Beat = body_json(body)?;
    let mut s = api.sched.lock().unwrap();
    s.reap(now_us());
    s.register(b.caps.clone());
    let leases = s.heartbeat(&b.caps.id)?;
    Ok(Reply::json(&serde_json::json!({ "leases": leases })))
}

fn lease(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let caps: WorkerCaps = body_json(body)?;
    let mut s = api.sched.lock().unwrap();
    s.reap(now_us());
    s.register(caps.clone());
    match s.lease(&caps.id) {
        Some(l) => {
            api.held.lock().unwrap().insert(l.task_id.clone(), l.clone());
            Ok(Reply::json(&l))
        }
        None => Ok(Reply { status: 204, ctype: "application/json", body: Vec::new(), headers: Vec::new() }),
    }
}

fn source_kind(k: &str) -> SourceKind {
    match k {
        "webcam" => SourceKind::Webcam,
        "archive" => SourceKind::Archive,
        "image" => SourceKind::Image,
        "folder" | "collection" => SourceKind::Collection,
        // "file" is the looping demo feed: it never finishes, so it schedules like a live camera.
        _ => SourceKind::Live,
    }
}

/// Register every configured camera as a Source + MediaAsset + IndexJob + scheduler task.
/// Cheap to call on every poll: unchanged cameras are skipped.
pub fn sync_sources(api: &Api, cams: &[Camera]) {
    let mut cat = api.catalog.lock().unwrap();
    for c in cams {
        let key = format!("{}|{}", c.url, c.kind);
        if cat.get(&c.id).is_some_and(|(k, _)| *k == key) {
            continue;
        }
        let kind = source_kind(&c.kind);
        let resolved = crate::watch::config::resolve_url(&c.url);
        let src = Source {
            id: c.id.clone(),
            name: c.name.clone(),
            kind,
            location: crate::watch::auth::redact(&c.url),
            credential_ref: None,
            eligible_workers: Vec::new(),
            enabled: true,
            capture_timezone: None,
        };
        let media = if matches!(kind, SourceKind::Live | SourceKind::Webcam) && c.kind != "file" {
            MediaAsset {
                id: stable_id("media", &[&c.id, "live", &src.location]),
                source_id: c.id.clone(),
                uri: src.location.clone(),
                fingerprint: format!("live:{}", c.id),
                duration_us: None, width: None, height: None, codec: None, timebase: None,
                capture_start_utc_us: None,
                proxy: ProxyStatus::None,
                // No recording is retained for live sources yet; replay is unavailable.
                availability: Availability::Unknown,
                still: false,
            }
        } else {
            match crate::watch::media_catalog::probe(&c.id, Path::new(&resolved)) {
                Ok(m) => m,
                Err(_) => crate::watch::media_catalog::missing_asset(&c.id, Path::new(&resolved)),
            }
        };
        let _ = api.store.upsert_source(&src);
        let _ = api.store.upsert_media(&media);
        let job_id = stable_id("job", &[&c.id, &media.id]);
        let task_id = stable_id("task", &[&c.id, &media.id]);
        let job = IndexJob {
            id: job_id.clone(),
            manifest_version: "camera/1".into(),
            media_ids: vec![media.id.clone()],
            model_versions: Vec::new(),
            sampling: serde_json::to_value(crate::watch::config::load_watch().process).unwrap_or_default(),
            state: JobState::Queued,
            checkpoint: Default::default(),
            coverage: Coverage::default(),
            error: (media.availability == Availability::Missing).then(|| ApiError::new("not_found", format!("media not found: {}", src.location))),
            created_utc_us: now_us(),
            finished_utc_us: None,
        };
        if api.store.job(&job_id).ok().flatten().is_none() {
            let _ = api.store.upsert_job(&job);
        }
        let spec = TaskSpec {
            task_id: task_id.clone(),
            source_id: c.id.clone(),
            media_id: media.id.clone(),
            kind,
            segment: [None, None],
            overlap_us: 0,
            // CW_DEVICES on the worker must list the same identifier (e.g. "0").
            device: (kind == SourceKind::Webcam).then(|| c.url.clone()),
            eligible_workers: Vec::new(),
            settings: serde_json::json!({ "camera_id": c.id }),
        };
        if media.availability != Availability::Missing {
            let _ = api.sched.lock().unwrap().submit(job, vec![spec]);
        }
        cat.insert(c.id.clone(), (key, task_id));
    }
}

/// Leases this worker holds, keyed by camera id, after handing it any new ones.
pub fn leases_for(api: &Api, worker: &str, host: &str, cores: usize, may_take: bool) -> HashMap<String, TaskLease> {
    let mut s = api.sched.lock().unwrap();
    s.reap(now_us());
    if !s.workers().iter().any(|w| w.caps.id == worker) {
        // A worker that has not heartbeated yet; its own heartbeat replaces these caps.
        s.register(WorkerCaps { id: worker.into(), host: host.into(), cores: (cores * 4).max(4) as u32, decoders: vec!["ffmpeg".into()], ..Default::default() });
    }
    for id in s.rebalance() {
        audit(api, Some("scheduler"), "task.rebalance", &[&id], "ok", "moved to an idle worker; resumes from checkpoint");
    }
    let _ = s.heartbeat(worker);
    let mut held = api.held.lock().unwrap();
    while may_take {
        match s.lease(worker) {
            Some(mut l) => {
                resume_from_checkpoint(api, &mut l);
                held.insert(l.task_id.clone(), l);
            }
            None => break,
        }
    }
    let live: std::collections::HashSet<String> = s.tasks().into_iter().filter(|t| t.state == "leased" && t.worker.as_deref() == Some(worker)).map(|t| t.task_id).collect();
    held.retain(|id, l| live.contains(id) || l.worker != worker);
    held.values()
        .filter(|l| l.worker == worker && live.contains(&l.task_id))
        .filter_map(|l| l.settings.get("camera_id").and_then(|c| c.as_str()).map(|c| (c.to_string(), l.clone())))
        .collect()
}

/// A reassigned archive resumes at its last committed media time instead of the file start.
/// Two seconds before it are decoded for tracker warm-up and not indexed (`ingest::owns`).
/// Track IDs restart in the new lease: identity does not carry across a failover.
fn resume_from_checkpoint(api: &Api, l: &mut TaskLease) {
    let archive = api.store.media(&l.media_id).ok().flatten().is_some_and(|m| m.duration_us.is_some() && !m.still)
        && l.settings.get("source_id").and_then(|s| s.as_str()).and_then(|id| api.store.sources().ok()?.into_iter().find(|s| s.id == id)).is_some_and(|s| s.kind == SourceKind::Archive);
    if !archive || l.segment[0].is_some() {
        return;
    }
    if let Some(&at) = api.store.job(&l.job_id).ok().flatten().and_then(|j| j.checkpoint.get(&l.media_id).copied()).as_ref() {
        if at > 0 {
            l.segment[0] = Some(at);
            l.overlap_us = 2_000_000;
        }
    }
}

/// Worker-reported camera state: finished archives complete their task; permanent errors fail it.
pub fn camera_state(api: &Api, worker: &str, camera: &str, state: &str, error_code: &str, coverage: Option<&serde_json::Value>) {
    let lease = api.held.lock().unwrap().values().find(|l| l.worker == worker && l.settings.get("camera_id").and_then(|c| c.as_str()) == Some(camera)).cloned();
    let Some(l) = lease else { return };
    if let Some(cov) = coverage.and_then(|c| serde_json::from_value::<Coverage>(c.clone()).ok()) {
        if let Ok(Some(mut j)) = api.store.job(&l.job_id) {
            j.coverage = cov;
            j.state = match state {
                "completed" => JobState::Completed,
                "error" => JobState::Failed,
                _ => JobState::Running,
            };
            if state == "error" && !error_code.is_empty() {
                j.error = Some(ApiError::new(error_code, format!("worker {worker} reported {error_code}")));
            }
            if j.state == JobState::Completed && j.finished_utc_us.is_none() {
                j.finished_utc_us = Some(now_us());
            }
            let _ = api.store.upsert_job(&j);
        }
    }
    let mut s = api.sched.lock().unwrap();
    match state {
        "completed" => {
            let _ = s.complete(&l.task_id, l.generation);
        }
        "error" if !error_code.is_empty() => {
            let permanent = matches!(error_code, "model_unavailable" | "missing_file" | "unsupported_media" | "not_found");
            let _ = s.fail_with(&l.task_id, l.generation, permanent, ApiError::new(error_code, format!("worker {worker} reported {error_code}")));
        }
        _ => {}
    }
}

fn run_query(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let req: q::Request = body_json(body)?;
    let o = q::run_detailed(&*api.store, &req)?;
    let (page, next) = q::matches_page(&*api.store, &o.run.id, None, 200)?;
    let match_count = o.matches.len();
    audit(api, req.actor.as_deref(), "query.run", &[&o.run.id], "ok", &format!("{} ({} {})", o.run.normalized, o.run.result_count, o.unit));
    Ok(Reply::json(&serde_json::json!({ "run": o.run, "match_count": match_count, "matches": page, "next": next, "unit": o.unit, "distinct_tracks": o.distinct_tracks, "values": o.values })))
}

/// Validate every configured module against its artifact and record the version.
pub fn register_models(api: &Api) -> Vec<(String, Result<ModelVersion, ApiError>)> {
    let modules: Vec<_> = crate::watch::config::load_modules().into_iter().filter(|m| m.kind != "fight").collect();
    // Validation hashes the artifact and loads it once; redo it only when a module or its file changed.
    let key: String = modules
        .iter()
        .map(|m| {
            let meta = std::fs::metadata(crate::watch::config::resolve_url(&m.model)).ok();
            let stamp = meta.map(|x| (x.len(), x.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)));
            format!("{}|{}|{}|{:?};", m.id, m.model, m.enabled, stamp)
        })
        .collect();
    if let Some((k, v)) = api.model_cache.lock().unwrap().as_ref() {
        if *k == key {
            return v.clone();
        }
    }
    let out: Vec<_> = modules.into_iter().map(|m| (m.id.clone(), crate::watch::models::validate(&m))).collect();
    for (_, r) in &out {
        if let Ok(mv) = r {
            let _ = api.store.upsert_model(mv);
        }
    }
    *api.model_cache.lock().unwrap() = Some((key, out.clone()));
    out
}

fn models(api: &Api) -> Result<Reply, ApiError> {
    let checked = register_models(api);
    let installed = api.store.models()?;
    let modules = crate::watch::config::load_modules();
    let rows: Vec<_> = checked
        .into_iter()
        .map(|(id, r)| {
            let m = modules.iter().find(|m| m.id == id);
            match r {
                Ok(mv) => serde_json::json!({ "module": m, "model": mv, "status": "ok" }),
                Err(e) => serde_json::json!({ "module": m, "model": null, "status": "error", "error": e }),
            }
        })
        .collect();
    let support = |p| crate::watch::models::support(&installed, p);
    Ok(Reply::json(&serde_json::json!({
        "models": rows,
        "support": { "near": support(Predicate::Near), "holding": support(Predicate::Holding) },
        "notes": { "near": "Spatial proximity heuristic; does not prove holding.", "confidence": "Detector score, not a probability of correctness." },
    })))
}

#[derive(serde::Deserialize)]
struct DecisionIn {
    decision: Decision,
    #[serde(default)]
    actor: Option<String>,
}

fn decide(api: &Api, id: &str, body: &[u8]) -> Result<Reply, ApiError> {
    let d: DecisionIn = body_json(body)?;
    let actor = d.actor.unwrap_or_else(|| "local".into());
    let mut eng = api.engine.lock().unwrap();
    let a = match eng.decide(id, d.decision, &actor) {
        Ok(a) => a,
        Err(_) => {
            // ponytail: linear page scan for an alert the engine no longer holds; add Store::alert(id) if it grows.
            let mut after = None;
            let found = loop {
                let (list, next) = api.store.alerts(&Page { after: after.clone(), limit: 1000 })?;
                if let Some(a) = list.into_iter().find(|a| a.id == id) {
                    break Some(a);
                }
                if next.is_none() {
                    break None;
                }
                after = next;
            };
            let mut a = found.ok_or_else(|| missing("alert"))?;
            alerts::apply_decision(&mut a, d.decision, &actor, now_us())?;
            eng.adopt(a.clone());
            a
        }
    };
    drop(eng);
    api.store.upsert_alert(&a)?;
    audit(api, Some(&actor), "alert.decision", &[id], "ok", &format!("{:?}", d.decision));
    api.save_alert_state();
    Ok(Reply::json(&a))
}

#[derive(serde::Deserialize)]
struct BacktestIn {
    #[serde(default)]
    spec: Option<alerts::RuleSpec>,
    /// Legacy rule shape from the Rules page.
    #[serde(default)]
    rule: Option<crate::watch::config::Rule>,
    #[serde(default)]
    scope: Option<QueryScope>,
}

fn backtest(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let b: BacktestIn = body_json(body)?;
    let spec = match (b.spec, b.rule) {
        (Some(s), _) => s,
        (None, Some(r)) => alerts::RuleSpec::from_legacy(&r)?,
        _ => return Err(bad("spec or rule required")),
    };
    spec.validate()?;
    let scope = b.scope.unwrap_or(QueryScope { source_ids: vec![], media_ids: vec![], from_utc_us: None, to_utc_us: None });
    let rep = alerts::backtest_report(&*api.store, &spec, &scope)?;
    Ok(Reply::json(&serde_json::json!({ "alerts": rep.alerts, "diagnostics": rep.diagnostics, "observations": rep.observations })))
}

#[derive(serde::Deserialize)]
struct SourceIn {
    id: String,
    name: String,
    kind: String,
    location: String,
    #[serde(default)]
    credential: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

/// Create or update a source. Writes watch.json (the pipeline's source of truth) and registers it.
fn save_source(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let s: SourceIn = body_json(body)?;
    if s.credential.as_deref().is_some_and(|c| !c.is_empty()) {
        return Err(ApiError::new("unsupported", "credential storage is not implemented; keep credentials out of the location and configure them on the camera network"));
    }
    if s.id.is_empty() || !s.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(bad("source id must contain only letters, digits, underscores or hyphens"));
    }
    let kind = match s.kind.as_str() {
        "collection" => "folder",
        k @ ("live" | "webcam" | "file" | "archive" | "image" | "folder") => k,
        _ => return Err(bad("kind must be live, webcam, archive, image, folder/collection or file")),
    };
    if s.name.trim().is_empty() || s.location.trim().is_empty() {
        return Err(bad("name and location are required"));
    }
    if s.location.contains("://") && s.location.split("://").nth(1).is_some_and(|r| r.split('/').next().unwrap_or("").contains('@')) {
        return Err(bad("remove user:password from the location; credentials must not be stored in source URLs"));
    }
    let path = crate::watch::config::watch_path();
    let mut v: serde_json::Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    let cams = v["cameras"].as_array().cloned().unwrap_or_default();
    let mut out: Vec<serde_json::Value> = cams.into_iter().filter(|c| c["id"].as_str() != Some(&s.id)).collect();
    if s.enabled.unwrap_or(true) {
        out.push(serde_json::json!({ "id": s.id, "name": s.name, "url": s.location, "kind": kind, "start": 0.0 }));
    }
    v["cameras"] = serde_json::Value::Array(out);
    v["configured"] = serde_json::json!(true);
    crate::watch::config::save_json(&path, &v).map_err(|e| ApiError::new("internal", e))?;
    sync_sources(api, &crate::watch::config::load_watch().cameras);
    audit(api, None, "sources.save", &[&s.id], "ok", &format!("{} {}", kind, crate::watch::auth::redact(&s.location)));
    let src = api.store.sources()?.into_iter().find(|x| x.id == s.id);
    let job = api.store.jobs()?.into_iter().find(|j| j.id.starts_with("job_") && src.is_some() && api.store.media(j.media_ids.first().map(String::as_str).unwrap_or("")).ok().flatten().is_some_and(|m| m.source_id == s.id));
    Ok(Reply::json(&serde_json::json!({ "source": src, "job": job })))
}

/// Media as shown to browsers: the location is the path relative to the data root, never an absolute path.
fn public_media(mut m: MediaAsset) -> MediaAsset {
    let root = crate::watch::repo_root();
    let p = Path::new(&m.uri);
    m.uri = p.strip_prefix(&root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| {
        if p.is_absolute() { p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default() } else { crate::watch::auth::redact(&m.uri) }
    });
    m
}

/// Timeline additions for the review UI: `recorded:false` for live lanes with no retained footage,
/// absolute paths removed from lane media, and adjacent gaps merged.
fn tidy_timeline(api: &Api, mut v: serde_json::Value) -> serde_json::Value {
    if let Some(lanes) = v.get_mut("lanes").and_then(|l| l.as_array_mut()) {
        for lane in lanes {
            let Some(mid) = lane.get("media_id").and_then(|m| m.as_str()).map(String::from) else { continue };
            if let Ok(Some(m)) = api.store.media(&mid) {
                let live = m.duration_us.is_none() && !m.still;
                lane["recorded"] = serde_json::json!(!live);
                if let Some(obj) = lane.get_mut("media").filter(|x| x.is_object()) {
                    *obj = serde_json::to_value(public_media(m)).unwrap_or_default();
                }
            }
        }
    }
    if let Some(gaps) = v.get_mut("gaps").and_then(|g| g.as_array_mut()) {
        let mut list: Vec<(String, Us, Us, serde_json::Value)> = gaps
            .iter()
            .filter_map(|g| Some((g.get("media_id")?.as_str()?.to_string(), g.get("start_us")?.as_i64()?, g.get("end_us")?.as_i64()?, g.clone())))
            .collect();
        list.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        let mut merged: Vec<(String, Us, Us, serde_json::Value)> = Vec::new();
        for g in list {
            match merged.last_mut() {
                Some(last) if last.0 == g.0 && g.1 <= last.2 + 1 => last.2 = last.2.max(g.2),
                _ => merged.push(g),
            }
        }
        *gaps = merged.into_iter().map(|(_, a, b, mut g)| { g["start_us"] = a.into(); g["end_us"] = b.into(); g }).collect();
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_gaps_merge_and_live_lanes_are_unrecorded() {
        let api = Api::new(crate::watch::storage::memory(), std::env::temp_dir(), crate::watch::auth::Auth::load(&std::env::temp_dir().join("cw-api-test")));
        api.store.upsert_media(&MediaAsset {
            id: "live1".into(), source_id: "s".into(), uri: "rtsp://cam".into(), fingerprint: "f".into(), duration_us: None,
            width: None, height: None, codec: None, timebase: None, capture_start_utc_us: None, proxy: ProxyStatus::None,
            availability: Availability::Unknown, still: false,
        }).unwrap();
        let v = tidy_timeline(&api, serde_json::json!({
            "lanes": [{"media_id": "live1"}],
            "gaps": [{"media_id":"m","start_us":0,"end_us":10},{"media_id":"m","start_us":10,"end_us":20},{"media_id":"m","start_us":50,"end_us":60}],
        }));
        assert_eq!(v["lanes"][0]["recorded"], false);
        assert_eq!(v["gaps"].as_array().unwrap().len(), 2);
        assert_eq!(v["gaps"][0]["end_us"], 20);
    }

    #[test]
    fn public_media_hides_absolute_paths() {
        let m = MediaAsset {
            id: "m".into(), source_id: "s".into(), uri: "/Users/x/secret/clip.mp4".into(), fingerprint: "f".into(), duration_us: Some(1),
            width: None, height: None, codec: None, timebase: None, capture_start_utc_us: None, proxy: ProxyStatus::None,
            availability: Availability::Available, still: false,
        };
        assert_eq!(public_media(m).uri, "clip.mp4");
    }
}
