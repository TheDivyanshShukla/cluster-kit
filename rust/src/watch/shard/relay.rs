//! Head side of a sharded deployment: the commit receipt (`/shard/ack`), the event relay (`/shard/events`), and the
//! media proxy. Node side: the pusher that sends `index` and `alert` events to the head and resumes from the head's cursor.
//!
//! Events carry a per-node sequence. The head applies each sequence once and remembers the last one per node
//! (`shard_cursor.json`), so a retry, a node restart or a head restart neither loses nor repeats an event. Alert ids
//! are deterministic, so even a replay after the cursor file is lost only rewrites the same alert row.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::*;
use crate::watch::api;

/// What a node tells the head about a batch it already committed locally.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub node_id: String,
    #[serde(default)]
    pub lease: Option<LeaseRef>,
    pub observation: FrameObservation,
    /// Class, confidence, box, track and color of each detection, for the live overlay.
    #[serde(default)]
    pub detections: Vec<serde_json::Value>,
    /// The inference frame. The head keeps thumbnails (small); detections stay on the node.
    #[serde(default)]
    pub thumbnail_hex: Option<String>,
    pub snapshot: u64,
}

fn now_us() -> Us {
    (now_s() * 1e6) as Us
}

pub fn ack(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let r: Receipt = serde_json::from_slice(body).map_err(|e| ApiError::new("invalid_request", format!("invalid JSON: {e}")))?;
    let o = &r.observation;
    if o.observation_id.is_empty() || o.media_id.is_empty() || o.source_id.is_empty() || r.node_id.is_empty() {
        return Err(ApiError::new("invalid_request", "node_id, observation_id, media_id and source_id are required"));
    }
    if let Some(l) = &r.lease {
        let mut s = api.sched.lock().unwrap();
        s.reap(now_us());
        s.check(l)?;
    }
    if let Some(hex) = r.thumbnail_hex.as_deref().filter(|h| !h.is_empty()) {
        api::save_thumbnail(api, &o.observation_id, hex)?;
    }
    // The checkpoint advances only now that the node reports the rows durable.
    if let Some(mut job) = api.store.job(&o.job_id)? {
        if o.media_time_us > job.checkpoint.get(&o.media_id).copied().unwrap_or(i64::MIN) {
            job.checkpoint.insert(o.media_id.clone(), o.media_time_us);
            api.store.upsert_job(&job)?;
        }
    }
    if let Some(mut m) = api.store.media(&o.media_id)? {
        if m.node_id.as_deref() != Some(r.node_id.as_str()) {
            m.node_id = Some(r.node_id.clone());
            api.store.upsert_media(&m)?;
        }
    }
    registry().note_media(&r.node_id, &o.media_id);
    if o.thumbnail.is_some() {
        let newer = api.latest.lock().unwrap().get(&o.source_id).and_then(|v| v["media_time_us"].as_i64()).map_or(true, |t0| o.media_time_us >= t0 || o.media_time_us + 5_000_000 < t0);
        if newer {
            api.latest.lock().unwrap().insert(o.source_id.clone(), serde_json::json!({
                "observation_id": o.observation_id, "media_id": o.media_id, "media_time_us": o.media_time_us,
                "frame": format!("/api/v1/observations/{}/thumbnail", o.observation_id), "detections": r.detections, "file": o.thumbnail,
            }));
        }
    }
    Ok(Reply::json(&serde_json::json!({ "ok": true })))
}

// ------------------------------------------------------------------ events

static CURSORS: Mutex<Option<(std::path::PathBuf, BTreeMap<String, i64>)>> = Mutex::new(None);

pub fn load_cursors(file: &std::path::Path) {
    let m: BTreeMap<String, i64> = std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    *CURSORS.lock().unwrap() = Some((file.to_path_buf(), m));
}

fn cursor_of(node: &str) -> i64 {
    CURSORS.lock().unwrap().as_ref().and_then(|(_, m)| m.get(node).copied()).unwrap_or(0)
}

fn set_cursor(node: &str, seq: i64) {
    let mut g = CURSORS.lock().unwrap();
    let (file, m) = g.get_or_insert_with(|| (data_dir().join("shard_cursor.json"), BTreeMap::new()));
    m.insert(node.to_string(), seq);
    if let Ok(b) = serde_json::to_vec(m) {
        let _ = std::fs::write(file.as_path(), b);
    }
}

#[derive(Deserialize)]
struct EventsIn {
    node: String,
    #[serde(default)]
    events: Vec<EventIn>,
}

#[derive(Deserialize)]
struct EventIn {
    seq: i64,
    kind: String,
    data: serde_json::Value,
}

/// Existing copy of an alert, so a node's later update keeps the human decision made on the head.
fn find_alert(api: &Api, id: &str) -> Option<Alert> {
    let mut after = None;
    for _ in 0..5 {
        let (list, next) = api.store.alerts(&Page { after: after.clone(), limit: 500 }).ok()?;
        if let Some(a) = list.into_iter().find(|a| a.id == id) {
            return Some(a);
        }
        after = Some(next?);
    }
    None
}

pub fn events(api: &Api, body: &[u8]) -> Result<Reply, ApiError> {
    let mut e: EventsIn = serde_json::from_slice(body).map_err(|e| ApiError::new("invalid_request", format!("invalid JSON: {e}")))?;
    e.events.sort_by_key(|x| x.seq);
    let mut last = cursor_of(&e.node);
    let start = last;
    for ev in e.events {
        if ev.seq <= last {
            continue; // already applied: a retry or a replay after a restart
        }
        match ev.kind.as_str() {
            "index" => api.bus.publish("index", &ev.data),
            "alert" => {
                let mut a: Alert = serde_json::from_value(ev.data.clone()).map_err(|e| ApiError::new("invalid_request", format!("bad alert event: {e}")))?;
                if let Some(old) = find_alert(api, &a.id) {
                    if old.decision != Decision::Unreviewed {
                        a.decision = old.decision;
                        a.actor = old.actor;
                    }
                }
                api.store.upsert_alert(&a)?;
                api.bus.publish("alert", &serde_json::to_value(&a).unwrap_or_default());
            }
            _ => {}
        }
        last = ev.seq;
    }
    if last != start {
        set_cursor(&e.node, last);
    }
    Ok(Reply::json(&serde_json::json!({ "acked": last })))
}

fn auth_header() -> Option<String> {
    std::env::var("CW_WORKER_TOKEN").ok().map(|t| format!("Bearer {t}"))
}

/// Node side. Asks the head where it stopped, then pushes everything after that and trims what the head has.
pub fn spawn_pusher(node: Arc<node::Node>) {
    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(3)).build();
        let url = format!("{}/api/v1/shard/events", node.head);
        let mut acked: Option<i64> = None;
        loop {
            std::thread::sleep(Duration::from_millis(400));
            let batch = acked.map(|a| node.shard.events_after(a, 200)).unwrap_or_default();
            if acked.is_some() && batch.is_empty() {
                continue;
            }
            let events: Vec<_> = batch.iter().map(|(s, k, d)| serde_json::json!({ "seq": s, "kind": k, "data": d })).collect();
            let mut req = agent.post(&url);
            if let Some(h) = auth_header() {
                req = req.set("Authorization", &h);
            }
            match req.send_json(serde_json::json!({ "node": node.id, "events": events })) {
                Ok(r) => {
                    let v: serde_json::Value = r.into_json().unwrap_or_default();
                    if let Some(a) = v["acked"].as_i64() {
                        acked = Some(a);
                        node.shard.trim_events(a);
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(800)),
            }
        }
    });
}

// ------------------------------------------------------------------ media proxy

/// Head `/api/v1/media/{id}/...`: forward to the computer that holds the recording. `None` = serve it locally.
pub fn proxy_media(api: &Api, id: &str, rest: &[&str], url: &str, range: Option<&str>) -> Option<Reply> {
    let m = api.store.media(id).ok()??;
    let owner = match m.node_id.clone().filter(|n| n != "head") {
        Some(n) => n,
        None => {
            if std::path::Path::new(&crate::watch::config::resolve_url(&m.uri)).is_file() {
                return None;
            }
            registry().owner_of(id)?
        }
    };
    let down = || Reply::err(unavailable(format!("unavailable on its computer ({owner})")));
    let Some(rec) = registry().get(&owner) else { return Some(down()) };
    if Registry::state_of(&rec) == State::Down || rec.secret.is_empty() {
        return Some(down());
    }
    let query = url.split_once('?').map(|(_, q)| format!("?{q}")).unwrap_or_default();
    let target = format!("{}/api/v1/shard/media/{id}/{}{query}", rec.url.trim_end_matches('/'), rest.join("/"));
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(2)).timeout(Duration::from_secs(30)).build();
    let mut req = agent.get(&target).set("Authorization", &format!("Bearer {}", rec.secret));
    if let Some(r) = range {
        req = req.set("Range", r);
    }
    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(ureq::Error::Transport(_)) => return Some(down()),
    };
    let ctype: &'static str = match resp.content_type() {
        "video/mp4" => "video/mp4",
        "image/jpeg" => "image/jpeg",
        "image/png" => "image/png",
        "image/webp" => "image/webp",
        "application/json" => "application/json",
        _ => "application/octet-stream",
    };
    let status = resp.status();
    let headers: Vec<(String, String)> = ["Content-Range", "Accept-Ranges", "Cache-Control"].iter().filter_map(|h| resp.header(h).map(|v| (h.to_string(), v.to_string()))).collect();
    let mut body = Vec::new();
    use std::io::Read;
    if resp.into_reader().take(64 * 1024 * 1024).read_to_end(&mut body).is_err() {
        return Some(down());
    }
    Some(Reply { status, ctype, body, headers })
}
