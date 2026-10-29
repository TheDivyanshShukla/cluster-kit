//! What the head knows about the PCs in the cluster: who they are, how busy, and how each camera is doing.
//!
//! Workers post a stats packet once a second (`/api/stats`) and poll `/api/work`; both feed this registry.
//! Nothing here is estimated from a model of the hardware. Rates are exponential averages of what the workers
//! counted, and the capacity figure is the inverse of the measured average cost of one sampled frame.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::watch::api::Api;
use crate::watch::now_secs;

/// A worker that has not been heard from for this long is lost (the lease length).
pub const LOST_AFTER: f64 = 15.0;
/// A live camera that has been behind for this long may be moved to an idle worker.
pub const BEHIND_FOR: f64 = 20.0;
/// Live camera lag above this many seconds, or any sustained dropping, means "behind".
const LAG_LIMIT: f64 = 2.5;
const DROP_LIMIT: f64 = 0.5;

fn ema(prev: Option<f64>, x: f64) -> f64 {
    prev.map_or(x, |p| p * 0.7 + x * 0.3)
}

#[derive(Clone, Default)]
pub struct CamStat {
    pub name: String,
    pub source_id: String,
    pub state: String,
    pub error: String,
    pub fps: f64,
    pub arrive: f64,
    pub dropped: f64,
    pub lag_s: f64,
    pub at: f64,
    pub dur: f64,
    pub decoded: u64,
    pub inferred: u64,
    pub skipped: u64,
    pub failed: u64,
    pub passes: u64,
    pub pixels_inferred: u64,
    pub pixels_full: u64,
    pub model_runs: u64,
    pub seen: f64,
    pub started: f64,
    pub behind_since: Option<f64>,
}

#[derive(Clone, Default)]
pub struct Node {
    pub id: String,
    pub host: String,
    pub ip: String,
    pub os: String,
    pub arch: String,
    pub label: String,
    pub cores: usize,
    pub local: bool,
    pub first_seen: f64,
    pub seen: f64,
    pub cpu: f64,
    pub ram_used: f64,
    pub ram_total: f64,
    pub measured_fps: f64,
    pub frame_ms: f64,
    pub cams: HashMap<String, CamStat>,
}

impl Node {
    pub fn state(&self, now: f64) -> &'static str {
        let age = now - self.seen;
        if age > LOST_AFTER {
            "lost"
        } else if age > 6.0 {
            "slow"
        } else if self.cams.values().any(|c| c.state == "running" && now - c.seen < 5.0) {
            "busy"
        } else {
            "idle"
        }
    }
}

#[derive(Clone)]
pub struct Move {
    pub ts: f64,
    pub camera: String,
    pub from: String,
    pub why: String,
}

#[derive(Default)]
pub struct Fleet {
    pub nodes: HashMap<String, Node>,
    pub moves: Vec<Move>,
    pub last_move: f64,
}

pub fn note_poll(api: &Api, id: &str, host: &str, cores: usize, local: bool, label: &str) {
    let now = now_secs();
    let mut f = api.fleet.lock().unwrap();
    let n = f.nodes.entry(id.to_string()).or_insert_with(|| Node { id: id.into(), first_seen: now, ..Default::default() });
    n.host = host.into();
    n.cores = cores;
    n.local = local;
    n.seen = now;
    if !label.is_empty() {
        n.label = label.into();
    }
}

/// One `/api/stats` packet. Per-second counters are averaged; cumulative ones are kept as reported.
pub fn note_stats(api: &Api, v: &Value) {
    let Some(id) = v["id"].as_str() else { return };
    let now = now_secs();
    let mut f = api.fleet.lock().unwrap();
    let n = f.nodes.entry(id.to_string()).or_insert_with(|| Node { id: id.into(), first_seen: now, ..Default::default() });
    let s = |k: &str| v[k].as_str().map(str::to_string);
    if let Some(x) = s("host") { n.host = x }
    if let Some(x) = s("ip") { n.ip = x }
    if let Some(x) = s("os") { n.os = x }
    if let Some(x) = s("arch") { n.arch = x }
    if let Some(x) = s("label").filter(|x| !x.is_empty()) { n.label = x }
    if let Some(c) = v["cores"].as_u64() { n.cores = c.max(1) as usize }
    n.local = v["local"].as_bool().unwrap_or(n.local);
    n.seen = now;
    n.cpu = ema(Some(n.cpu).filter(|c| *c > 0.0), v["cpu"].as_f64().unwrap_or(0.0));
    n.ram_used = v["ram_used"].as_f64().unwrap_or(n.ram_used);
    n.ram_total = v["ram_total"].as_f64().unwrap_or(n.ram_total);
    n.measured_fps = v["measured_fps"].as_f64().unwrap_or(n.measured_fps);
    n.frame_ms = v["frame_ms"].as_f64().filter(|m| *m > 0.0).unwrap_or(n.frame_ms);
    let dt = v["dt"].as_f64().unwrap_or(1.0).max(0.2);
    let Some(cams) = v["cameras"].as_object() else { return };
    for (key, c) in cams {
        let e = n.cams.entry(key.clone()).or_insert_with(|| CamStat { started: now, ..Default::default() });
        let rate = |k: &str| c[k].as_f64().unwrap_or(0.0) / dt;
        e.source_id = key.split('~').next().unwrap_or(key).to_string();
        e.name = c["name"].as_str().unwrap_or(key).to_string();
        e.state = c["state"].as_str().unwrap_or("running").to_string();
        e.error = c["error"].as_str().unwrap_or("").to_string();
        let processed = rate("processed");
        let dropped = rate("missed");
        e.fps = ema(Some(e.fps).filter(|x| *x > 0.0 || e.seen > 0.0), processed);
        e.dropped = ema(Some(e.dropped).filter(|_| e.seen > 0.0), dropped);
        e.arrive = e.fps + e.dropped;
        let pace = c["pace"].as_f64().unwrap_or(25.0).max(1.0);
        e.lag_s = c["queue_age_ms"].as_f64().unwrap_or(0.0) / 1000.0 + c["debt"].as_f64().unwrap_or(0.0).max(0.0) / pace;
        e.at = c["at"].as_f64().unwrap_or(e.at);
        e.dur = c["dur"].as_f64().unwrap_or(e.dur);
        let cov = &c["coverage"];
        e.decoded = cov["decoded"].as_u64().unwrap_or(e.decoded);
        e.inferred = cov["inferred"].as_u64().unwrap_or(e.inferred);
        e.skipped = cov["skipped"].as_u64().unwrap_or(e.skipped);
        e.failed = cov["failed"].as_u64().unwrap_or(e.failed);
        let ops = &c["ops"];
        e.passes = ops["detector_passes"].as_u64().unwrap_or(e.passes);
        e.pixels_inferred = ops["pixels_inferred"].as_u64().unwrap_or(e.pixels_inferred);
        e.pixels_full = ops["pixels_full"].as_u64().unwrap_or(e.pixels_full);
        e.model_runs = ops["model_runs"].as_u64().unwrap_or(e.model_runs);
        e.seen = now;
        let behind = e.state == "running" && (e.dropped > DROP_LIMIT || e.lag_s > LAG_LIMIT);
        e.behind_since = if behind { e.behind_since.or(Some(now)) } else { None };
    }
}

fn is_live(kind: &str) -> bool {
    matches!(kind, "live" | "webcam" | "file")
}

/// The configured sources that are cameras (never archives or benchmarks), with their kind.
fn live_kinds() -> HashMap<String, (String, String)> {
    crate::watch::config::load_watch().cameras.into_iter().filter(|c| is_live(&c.kind) && c.analyze).map(|c| (c.id, (c.name, c.kind))).collect()
}

fn capacity(n: &Node) -> Option<f64> {
    (n.frame_ms > 0.0).then(|| 1000.0 / n.frame_ms)
}

/// Per-camera and per-worker view of the live cameras, plus total capacity against demand.
pub fn realtime(api: &Api) -> Value {
    let now = now_secs();
    let kinds = live_kinds();
    let f = api.fleet.lock().unwrap();
    // A camera that just moved is still in its old PC's last report for a few seconds: show it under the PC that holds the lease.
    let owner: HashMap<String, String> = api.held.lock().unwrap().values().filter_map(|l| Some((l.settings["camera_id"].as_str()?.to_string(), l.worker.clone()))).collect();
    let mut cams = Vec::new();
    let mut workers = Vec::new();
    let (mut cap_total, mut demand_total, mut measured_all) = (0.0, 0.0, true);
    let mut nodes: Vec<&Node> = f.nodes.values().filter(|n| now - n.seen <= LOST_AFTER).collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    for n in &nodes {
        let mut demand = 0.0;
        let mut count = 0;
        for (key, c) in &n.cams {
            let Some((name, kind)) = kinds.get(key) else { continue };
            if owner.get(key).is_some_and(|w| *w != n.id) {
                continue;
            }
            if !matches!(c.state.as_str(), "running" | "error") || now - c.seen > 5.0 {
                continue;
            }
            if c.state == "running" {
                count += 1;
                demand += c.arrive;
            }
            let status = if !c.error.is_empty() {
                "error"
            } else if c.fps <= 0.0 && now - c.started < 20.0 {
                "starting"
            } else if c.behind_since.is_some() {
                "behind"
            } else {
                "keeping up"
            };
            cams.push(json!({
                "id": key, "name": name, "kind": kind, "worker": n.id, "host": n.host, "status": status, "error": c.error,
                "fps": round(c.fps), "required_fps": round(c.arrive), "dropped_per_s": round(c.dropped), "lag_s": round(c.lag_s),
                "behind_for_s": c.behind_since.map(|t| round(now - t)),
            }));
        }
        let cap = capacity(n);
        if count > 0 && cap.is_none() {
            measured_all = false;
        }
        cap_total += cap.unwrap_or(0.0);
        demand_total += demand;
        workers.push(json!({
            "id": n.id, "host": n.host, "os": n.os, "cpu": round(n.cpu), "cameras": count, "frame_ms": round(n.frame_ms),
            "capacity_fps": cap.map(round), "demand_fps": round(demand), "headroom_fps": cap.map(|c| round(c - demand)),
            "load": cap.map(|c| round(demand / c.max(0.01))),
        }));
    }
    let moves: Vec<Value> = f.moves.iter().rev().take(8).map(|m| json!({"ts": m.ts, "camera": m.camera, "from": m.from, "why": m.why})).collect();
    json!({
        "ts": now,
        "cameras": cams,
        "workers": workers,
        "total": {
            "capacity_fps": round(cap_total), "demand_fps": round(demand_total), "headroom_fps": round(cap_total - demand_total),
            "workers": nodes.len(), "cameras": cams.len(), "capacity_measured": measured_all,
            "note": "Capacity is 1 / the measured average cost of one sampled frame on each PC. It moves with how busy the picture is.",
        },
        "moves": moves,
    })
}

fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Live task ids that have been behind long enough to move, for the scheduler's conservative rebalance.
pub fn behind_tasks(api: &Api) -> Vec<String> {
    let now = now_secs();
    let f = api.fleet.lock().unwrap();
    if now - f.last_move < 10.0 {
        return Vec::new();
    }
    let held = api.held.lock().unwrap();
    let mut out = Vec::new();
    for n in f.nodes.values() {
        for (key, c) in &n.cams {
            if c.behind_since.is_some_and(|t| now - t >= BEHIND_FOR) {
                if let Some(l) = held.values().find(|l| l.worker == n.id && l.settings["camera_id"].as_str() == Some(key.as_str())) {
                    out.push(l.task_id.clone());
                }
            }
        }
    }
    out
}

/// Record that a live camera was moved, and give it a fresh 20 s before it can be moved again.
pub fn moved(api: &Api, task: &str) {
    let now = now_secs();
    let from = api.held.lock().unwrap().get(task).map(|l| (l.worker.clone(), l.settings["camera_id"].as_str().unwrap_or("").to_string()));
    let mut f = api.fleet.lock().unwrap();
    f.last_move = now;
    if let Some((worker, cam)) = from {
        if let Some(c) = f.nodes.get_mut(&worker).and_then(|n| n.cams.get_mut(&cam)) {
            c.behind_since = None;
        }
        f.moves.push(Move { ts: now, camera: cam, from: worker, why: "behind for 20 s while another PC was idle; tracker ids restart on the new PC".into() });
        let extra = f.moves.len().saturating_sub(50);
        f.moves.drain(..extra);
    }
}

/// Everything the Cluster and Join pages list: the scheduler's workers and tasks, plus what each PC reports about itself.
pub fn cluster(api: &Api) -> Value {
    let now = now_secs();
    let (workers, tasks) = {
        let s = api.sched.lock().unwrap();
        (s.workers(), s.tasks())
    };
    let held = api.held.lock().unwrap().clone();
    let f = api.fleet.lock().unwrap();
    let mut ids: Vec<&String> = f.nodes.keys().collect();
    ids.sort();
    let nodes: Vec<Value> = ids
        .into_iter()
        .map(|id| {
            let n = &f.nodes[id];
            let devices = workers.iter().find(|w| w.caps.id == *id).map(|w| w.caps.devices.clone()).unwrap_or_default();
            let mine: Vec<Value> = tasks
                .iter()
                .filter(|t| t.worker.as_deref() == Some(id.as_str()) && t.state == "leased")
                .map(|t| {
                    let l = held.get(&t.task_id);
                    json!({
                        "task_id": t.task_id, "job_id": t.job_id, "generation": t.generation,
                        "camera": l.and_then(|l| l.settings["camera_id"].as_str()),
                        "source": l.and_then(|l| l.settings["source_id"].as_str()),
                        "segment_us": l.map(|l| l.segment),
                    })
                })
                .collect();
            json!({
                "id": n.id, "host": n.host, "ip": n.ip, "os": n.os, "arch": n.arch, "label": n.label, "cores": n.cores, "local": n.local,
                "joined": n.first_seen, "seen": n.seen, "state": n.state(now), "cpu": round(n.cpu),
                "ram_used": n.ram_used, "ram_total": n.ram_total,
                "measured_fps": round(n.measured_fps), "frame_ms": round(n.frame_ms), "capacity_fps": capacity(n).map(round),
                "devices": devices, "tasks": mine,
            })
        })
        .collect();
    json!({ "workers": workers, "tasks": tasks, "nodes": nodes, "mode": api.auth.mode() })
}

/// `POST /api/v1/sources/probe {url}`: can this PC reach a camera stream, and what is it? Five seconds, no credentials
/// accepted, stored or logged. The answer is always 200 with `ok` true or false and a plain-words reason.
pub fn probe_source(body: &[u8]) -> Result<crate::watch::api::Reply, crate::watch::domain::ApiError> {
    use crate::watch::domain::ApiError;
    let url = serde_json::from_slice::<Value>(body).ok().and_then(|v| v["url"].as_str().map(|u| u.trim().to_string())).unwrap_or_default();
    let fail = |m: &str| Ok(crate::watch::api::Reply::json(&json!({ "ok": false, "error": m, "url": crate::watch::auth::redact(&url) })));
    if url.is_empty() || url.len() > 2048 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ApiError::new("invalid_request", "give a stream address such as http://192.168.1.50:8080/video or rtsp://192.168.1.50:8554/live"));
    }
    let scheme = url.split("://").next().unwrap_or("");
    if !url.contains("://") || !matches!(scheme, "http" | "https" | "rtsp" | "rtsps" | "rtmp") {
        return Err(ApiError::new("invalid_request", "the address must start with http://, https://, rtsp:// or rtmp://"));
    }
    if url.split("://").nth(1).is_some_and(|r| r.split('/').next().unwrap_or("").contains('@')) {
        return fail("The address contains a user name or password. Passwords are not accepted or stored here; use a stream address that needs none (on a private network) or put the camera behind a gateway that logs in for you.");
    }
    let mut cmd = std::process::Command::new("ffprobe");
    cmd.args(["-v", "error", "-print_format", "json", "-show_streams", "-select_streams", "v:0"]);
    if scheme.starts_with("rtsp") {
        cmd.args(["-rtsp_transport", "tcp"]);
    }
    cmd.arg(&url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let t0 = std::time::Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(&format!("ffprobe did not start ({e}). Install ffmpeg and leave it on PATH.")),
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if t0.elapsed() > std::time::Duration::from_secs(5) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(40)),
            Err(_) => break None,
        }
    };
    let ms = t0.elapsed().as_millis() as u64;
    let mut out = String::new();
    let mut err = String::new();
    use std::io::Read;
    if let Some(mut o) = child.stdout.take() { let _ = o.read_to_string(&mut out); }
    if let Some(mut e) = child.stderr.take() { let _ = e.read_to_string(&mut err); }
    let Some(st) = status else {
        return fail("No answer within 5 seconds. Check the phone and this PC are on the same Wi-Fi, the stream app is running, and the address and port are right.");
    };
    let v: Value = serde_json::from_str(&out).unwrap_or_default();
    let stream = v["streams"].as_array().and_then(|a| a.first());
    match (st.success(), stream) {
        (true, Some(s)) => {
            let rate = |k: &str| s[k].as_str().and_then(|r| r.split_once('/')).and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?.max(1.0))).filter(|f| *f > 0.0 && f.is_finite());
            Ok(crate::watch::api::Reply::json(&json!({
                "ok": true, "url": crate::watch::auth::redact(&url), "width": s["width"], "height": s["height"], "codec": s["codec_name"],
                "fps": rate("avg_frame_rate").or_else(|| rate("r_frame_rate")).map(round), "latency_ms": ms,
            })))
        }
        _ => {
            let e = crate::watch::auth::redact_text(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no video stream found"));
            let low = e.to_ascii_lowercase();
            let why = if low.contains("refused") { "Nothing is listening at that address and port. Is the app's server started, and is the port right?" }
                else if low.contains("401") || low.contains("unauthorized") || low.contains("403") { "The stream wants a login. Credentials are not accepted here." }
                else if low.contains("404") || low.contains("not found") { "The server answered, but not at that path. Check the end of the address (for example /video or /live)." }
                else if low.contains("no route") || low.contains("unreachable") || low.contains("name or service") || low.contains("nodename") { "That address cannot be reached from this PC. Same Wi-Fi? Is the phone's IP address current?" }
                else { "ffprobe could not read a video stream from that address." };
            fail(&format!("{why} ({})", e.chars().take(160).collect::<String>()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(id: &str, key: &str, processed: u64, missed: u64, queue_ms: u64) -> Value {
        json!({"id": id, "host": "h", "os": "macos", "arch": "aarch64", "cores": 8, "cpu": 40.0, "frame_ms": 50.0, "measured_fps": 10.0,
               "cameras": {key: {"name": "Cam", "state": "running", "processed": processed, "missed": missed, "queue_age_ms": queue_ms, "pace": 25.0, "debt": 0.0,
                                  "coverage": {"decoded": 100, "inferred": 40, "skipped": 60, "failed": 0}, "ops": {"detector_passes": 30, "pixels_inferred": 5, "pixels_full": 9}}}})
    }

    #[test]
    fn node_state_follows_the_lease_timeout() {
        let mut n = Node { seen: 100.0, ..Default::default() };
        assert_eq!(n.state(103.0), "idle");
        assert_eq!(n.state(108.0), "slow");
        assert_eq!(n.state(100.0 + LOST_AFTER + 1.0), "lost");
        n.cams.insert("c".into(), CamStat { state: "running".into(), seen: 102.0, ..Default::default() });
        assert_eq!(n.state(103.0), "busy");
    }

    #[test]
    fn dropping_frames_marks_a_camera_behind_and_recovery_clears_it() {
        let api = crate::watch::api::test_api();
        note_stats(&api, &stats("w1", "cam1", 4, 3, 100));
        {
            let f = api.fleet.lock().unwrap();
            let c = &f.nodes["w1"].cams["cam1"];
            assert!(c.behind_since.is_some(), "3 dropped frames a second is behind");
            assert_eq!(c.source_id, "cam1");
            assert_eq!((c.decoded, c.inferred, c.skipped), (100, 40, 60));
        }
        for _ in 0..12 {
            note_stats(&api, &stats("w1", "cam1", 12, 0, 50));
        }
        assert!(api.fleet.lock().unwrap().nodes["w1"].cams["cam1"].behind_since.is_none());
    }

    #[test]
    fn probe_refuses_passwords_and_odd_schemes_without_running_anything() {
        let r = probe_source(br#"{"url":"rtsp://user:pw@10.0.0.9/live"}"#).unwrap();
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["ok"], false);
        assert!(!v.to_string().contains("pw@"), "the password never comes back");
        assert!(probe_source(br#"{"url":"file:///etc/passwd"}"#).is_err());
        assert!(probe_source(br#"{"url":"http://a b"}"#).is_err());
        assert!(probe_source(b"{}").is_err());
    }

    #[test]
    fn segment_keys_map_back_to_their_source() {
        let api = crate::watch::api::test_api();
        note_stats(&api, &stats("w1", "bench-a-0~3", 5, 0, 0));
        assert_eq!(api.fleet.lock().unwrap().nodes["w1"].cams["bench-a-0~3"].source_id, "bench-a-0");
    }
}
