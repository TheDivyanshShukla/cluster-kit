//! Node side: the local shard store, the shard read API (`/api/v1/shard/*`), and the worker hooks.
//!
//! A node is a worker. With `CW_SHARDS=1` it commits every batch to its own `shard.db` (same schema and
//! migrations as the head store), serves reads of it to the head, and only sends the head a small receipt.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags};
use serde::de::DeserializeOwned;

use super::*;
use crate::watch::alerts;
use crate::watch::api;
use crate::watch::config;
use crate::watch::storage::sqlite::Sqlite;

// ------------------------------------------------------------------ the local shard

pub struct LocalShard {
    pub id: String,
    pub store: Arc<dyn Store>,
    raw: Mutex<Connection>,
    /// Honour per-node snapshots (shard files). The head catalog answers as "everything it has".
    track_snapshots: bool,
    engine: Mutex<alerts::Engine>,
    rules_sig: Mutex<String>,
    media_seen: Mutex<HashSet<String>>,
}

fn db(e: rusqlite::Error) -> ApiError {
    ApiError::new("internal", e.to_string())
}

impl LocalShard {
    /// A node's own shard file. Created and migrated on first open.
    pub fn open(id: &str, path: &Path) -> StoreResult<Self> {
        let store: Arc<dyn Store> = Arc::new(Sqlite::open(path)?);
        let raw = Connection::open(path).map_err(db)?;
        raw.busy_timeout(Duration::from_secs(5)).map_err(db)?;
        raw.execute_batch(
            "CREATE INDEX IF NOT EXISTS commits_obs ON commits(observation_id);
             CREATE TABLE IF NOT EXISTS shard_events(seq INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, data TEXT NOT NULL);",
        )
        .map_err(db)?;
        Ok(Self::build(id, store, raw, true))
    }

    /// An existing store (the head catalog) answering shard reads about itself.
    pub fn over(id: &str, store: Arc<dyn Store>, path: &Path) -> Self {
        let raw = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).or_else(|_| Connection::open_in_memory()).expect("sqlite connection");
        let _ = raw.busy_timeout(Duration::from_secs(5));
        Self::build(id, store, raw, false)
    }

    fn build(id: &str, store: Arc<dyn Store>, raw: Connection, track: bool) -> Self {
        LocalShard { id: id.into(), store, raw: Mutex::new(raw), track_snapshots: track, engine: Mutex::new(alerts::Engine::new(vec![])), rules_sig: Mutex::new(String::new()), media_seen: Mutex::new(HashSet::new()) }
    }

    fn raw(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.raw.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn max_seq(&self) -> u64 {
        self.raw().query_row("SELECT COALESCE(MAX(seq),0) FROM commits", [], |r| r.get::<_, i64>(0)).map(|n| n as u64).unwrap_or(0)
    }

    /// `Some(n)` only when reading at `n` would hide rows that exist now.
    fn limit_of(&self, r: &ShardReq) -> Option<u64> {
        if !self.track_snapshots {
            return None;
        }
        r.snapshot.filter(|s| *s < self.max_seq())
    }

    fn visible(&self, obs: &str, snap: u64) -> bool {
        self.raw().query_row("SELECT 1 FROM commits WHERE observation_id=?1 AND seq<=?2", params![obs, snap as i64], |_| Ok(())).is_ok()
    }

    pub fn commit(&self, b: &IndexBatch) -> StoreResult<BatchOutcome> {
        self.store.commit_batch(b)
    }

    /// Remember where a recording lives on this computer so the head can proxy it.
    pub fn note_media(&self, media_id: &str, source_id: &str, uri: &str) {
        if !self.media_seen.lock().unwrap().insert(media_id.to_string()) {
            return;
        }
        let ext = Path::new(uri).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        let _ = self.store.upsert_media(&MediaAsset {
            id: media_id.into(), source_id: source_id.into(), uri: uri.into(), fingerprint: String::new(), duration_us: None, width: None, height: None,
            codec: None, timebase: None, capture_start_utc_us: None, proxy: ProxyStatus::None, availability: Availability::Available,
            still: matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp" | "bmp"), node_id: Some(self.id.clone()),
        });
    }

    pub fn set_rules(&self, rules: &[config::Rule]) -> Vec<Alert> {
        let sig = rules.iter().filter(|r| r.enabled).map(|r| format!("{}:{}:{}:{}", r.id, r.line, r.hold, r.camera)).collect::<Vec<_>>().join("|");
        let mut cur = self.rules_sig.lock().unwrap();
        if *cur == sig {
            return vec![];
        }
        *cur = sig;
        let specs = rules.iter().filter(|r| r.enabled).filter_map(|r| alerts::RuleSpec::from_legacy(r).ok()).collect();
        self.engine.lock().unwrap().set_rules(specs)
    }

    /// Evaluate the alert rules on the frame just committed, next to the data.
    pub fn evaluate(&self, o: &FrameObservation) -> Vec<Alert> {
        let f = DetectionFilter { media_ids: vec![o.media_id.clone()], media_from_us: Some(o.media_time_us), media_to_us: Some(o.media_time_us), ..Default::default() };
        let Ok((sums, _)) = self.store.summaries(&f, &Page { after: None, limit: 4 }) else { return vec![] };
        let mut eng = self.engine.lock().unwrap();
        let mut out = Vec::new();
        for s in sums.iter().filter(|s| s.observation_id == o.observation_id) {
            out.extend(eng.observe(s));
        }
        let _ = eng.take_diagnostics();
        out
    }

    pub fn push_event(&self, kind: &str, data: &serde_json::Value) {
        let _ = self.raw().execute("INSERT INTO shard_events(kind, data) VALUES(?1, ?2)", params![kind, data.to_string()]);
    }

    pub fn events_after(&self, seq: i64, limit: usize) -> Vec<(i64, String, serde_json::Value)> {
        let c = self.raw();
        let Ok(mut st) = c.prepare_cached("SELECT seq, kind, data FROM shard_events WHERE seq>?1 ORDER BY seq LIMIT ?2") else { return vec![] };
        let Ok(rows) = st.query_map(params![seq, limit as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))) else { return vec![] };
        rows.flatten().map(|(s, k, d)| (s, k, serde_json::from_str(&d).unwrap_or_default())).collect()
    }

    pub fn trim_events(&self, upto: i64) {
        let _ = self.raw().execute("DELETE FROM shard_events WHERE seq<=?1", params![upto]);
    }

    fn rel_key(&self, relation_id: &str) -> Option<(String, i64)> {
        self.raw().query_row("SELECT media_id, media_time_us FROM relations WHERE relation_id=?1", params![relation_id], |r| Ok((r.get(0)?, r.get(1)?))).ok()
    }

    fn obs_of_relation_ok(&self, r: &Relation, lim: Option<u64>) -> bool {
        lim.map_or(true, |s| self.visible(&r.observation_id, s))
    }
}

fn row<T: serde::Serialize>(k: (String, i64, String), v: &T) -> Row {
    Row { k, v: serde_json::to_value(v).unwrap_or_default() }
}

impl NodeClient for LocalShard {
    fn page(&self, kind: Kind, r: &ShardReq, _d: Duration) -> StoreResult<Rows> {
        let lim = self.limit_of(r);
        let snapshot = r.snapshot.unwrap_or_else(|| self.max_seq());
        let (rows, next) = match kind {
            Kind::Detections => {
                let (v, next) = self.store.detections(&r.filter, &r.page)?;
                let rows = v
                    .into_iter()
                    .filter(|(o, _)| lim.map_or(true, |s| self.visible(&o.observation_id, s)))
                    .map(|(o, d)| row((o.media_id.clone(), o.media_time_us, d.detection_id.clone()), &(&o, &d)))
                    .collect();
                (rows, next)
            }
            Kind::Summaries => {
                let (v, next) = self.store.summaries(&r.filter, &r.page)?;
                let rows = v
                    .into_iter()
                    .filter(|s| lim.map_or(true, |l| self.visible(&s.observation_id, l)))
                    .map(|s| row((s.media_id.clone(), s.media_time_us, s.observation_id.clone()), &s))
                    .collect();
                (rows, next)
            }
            Kind::Relations => {
                let (v, next) = self.store.relations(&r.filter, r.predicate, &r.page)?;
                let rows = v
                    .into_iter()
                    .filter(|x| self.obs_of_relation_ok(x, lim))
                    .filter_map(|x| self.rel_key(&x.relation_id).map(|(m, t)| row((m, t, x.relation_id.clone()), &x)))
                    .collect();
                (rows, next)
            }
        };
        Ok(Rows { rows, next, snapshot })
    }

    fn observations(&self, media_id: &str, from_us: Us, to_us: Us, _d: Duration) -> StoreResult<Vec<ObsRow>> {
        self.store.observations(media_id, from_us, to_us)
    }

    fn count(&self, r: &ShardReq, _d: Duration) -> StoreResult<u64> {
        if self.limit_of(r).is_none() {
            return self.store.count_detections(&r.filter);
        }
        let (mut n, mut after) = (0u64, None);
        loop {
            let req = ShardReq { page: Page { after: after.clone(), limit: 10_000 }, ..r.clone() };
            let rows = self.page(Kind::Detections, &req, Duration::ZERO)?;
            n += rows.rows.len() as u64;
            match rows.next {
                Some(c) => after = Some(c),
                None => return Ok(n),
            }
        }
    }

    fn status(&self, _d: Duration) -> StoreResult<NodeStatus> {
        let c = self.raw();
        let media: Vec<String> = c.prepare_cached("SELECT id FROM media ORDER BY id").and_then(|mut st| st.query_map([], |r| r.get(0)).map(|r| r.flatten().collect())).unwrap_or_default();
        let observations = c.query_row("SELECT COALESCE(MAX(rowid),0) FROM observations", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64;
        drop(c);
        Ok(NodeStatus { node_id: self.id.clone(), snapshot: self.store.snapshot()?, media, observations })
    }
}

// ------------------------------------------------------------------ head -> node over HTTP

pub struct HttpClient {
    base: String,
    secret: String,
    /// One pooled agent per node: keeps connections alive instead of opening a socket per call.
    agent: ureq::Agent,
}

impl HttpClient {
    pub fn new(base: &str, secret: &str) -> Self {
        HttpClient { base: base.trim_end_matches('/').into(), secret: secret.into(), agent: ureq::AgentBuilder::new().max_idle_connections_per_host(16).timeout_connect(Duration::from_secs(2)).build() }
    }

    fn call<T: DeserializeOwned>(&self, path: &str, body: Option<serde_json::Value>, d: Duration) -> StoreResult<T> {
        let url = format!("{}/api/v1/shard/{path}", self.base);
        let req = if body.is_some() { self.agent.post(&url) } else { self.agent.get(&url) }.timeout(d.max(Duration::from_millis(1))).set("Authorization", &format!("Bearer {}", self.secret));
        let resp = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match resp {
            Ok(r) => r.into_json::<T>().map_err(ie),
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                let e = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| serde_json::from_value::<ApiError>(v.get("error").cloned()?).ok());
                Err(e.unwrap_or_else(|| ApiError::new("unavailable", format!("node answered HTTP {code}"))))
            }
            Err(ureq::Error::Transport(t)) => Err(unavailable(format!("node unreachable: {t}"))),
        }
    }
}

impl NodeClient for HttpClient {
    fn page(&self, kind: Kind, r: &ShardReq, d: Duration) -> StoreResult<Rows> {
        let name = match kind {
            Kind::Detections => "detections",
            Kind::Summaries => "summaries",
            Kind::Relations => "relations",
        };
        self.call(name, Some(serde_json::to_value(r).map_err(ie)?), d)
    }
    fn observations(&self, media_id: &str, from_us: Us, to_us: Us, d: Duration) -> StoreResult<Vec<ObsRow>> {
        self.call("observations", Some(serde_json::json!({ "media_id": media_id, "from_us": from_us, "to_us": to_us })), d)
    }
    fn count(&self, r: &ShardReq, d: Duration) -> StoreResult<u64> {
        let v: serde_json::Value = self.call("count", Some(serde_json::to_value(r).map_err(ie)?), d)?;
        Ok(v["count"].as_u64().unwrap_or(0))
    }
    fn status(&self, d: Duration) -> StoreResult<NodeStatus> {
        self.call("status", None, d)
    }
}

// ------------------------------------------------------------------ node HTTP server

pub struct Node {
    pub shard: Arc<LocalShard>,
    pub id: String,
    pub url: String,
    pub secret: String,
    pub head: String,
    api: OnceLock<Arc<api::Api>>,
}

static NODE: OnceLock<Arc<Node>> = OnceLock::new();

pub fn global() -> Option<Arc<Node>> {
    NODE.get().cloned()
}

fn random_hex() -> String {
    use std::hash::{BuildHasher, Hasher};
    (0..4)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(i);
            format!("{:016x}", h.finish())
        })
        .collect()
}

/// worker.rs `run()`, after the head is found. No-op unless `CW_SHARDS=1`.
pub fn start(worker_id: &str, head_base: &str) {
    if !enabled() || NODE.get().is_some() {
        return;
    }
    let id = std::env::var("CW_NODE_ID").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| worker_id.to_string());
    let dir = data_dir();
    let path = std::env::var("CW_SHARD_DB").map(std::path::PathBuf::from).unwrap_or_else(|_| dir.join("shard.db"));
    let shard = match LocalShard::open(&id, &path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            crate::watch::log(&format!("shard store {}: {}", path.display(), e.message));
            return;
        }
    };
    let port0: u16 = std::env::var("CW_SHARD_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(7712);
    let (mut server, mut port) = (None, port0);
    for p in port0..port0.saturating_add(40) {
        if let Ok(s) = tiny_http::Server::http(("0.0.0.0", p)) {
            server = Some(s);
            port = p;
            break;
        }
    }
    let Some(server) = server else {
        crate::watch::log("shard: no free port for the shard server");
        return;
    };
    let url = std::env::var("CW_SHARD_ADVERTISE").unwrap_or_else(|_| format!("http://{}:{port}", crate::watch::primary_ip()));
    let node = Arc::new(Node { shard, id: id.clone(), url: url.clone(), secret: random_hex(), head: head_base.to_string(), api: OnceLock::new() });
    let _ = NODE.set(node.clone());
    crate::watch::log(&format!("shard node {id}: {} at {url}", path.display()));
    let n2 = node.clone();
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let n = n2.clone();
            std::thread::spawn(move || serve_one(&n, req));
        }
    });
    relay::spawn_pusher(node);
}

/// worker.rs heartbeat body: `"shard": shard::node::announce()`.
pub fn announce() -> Option<Announce> {
    global().map(|n| Announce { node_id: n.id.clone(), url: n.url.clone(), secret: n.secret.clone() })
}

/// worker.rs, where `/api/work` delivers rules.
pub fn set_rules(rules: &[config::Rule]) {
    if let Some(n) = global() {
        for a in n.shard.set_rules(rules) {
            n.shard.push_event("alert", &serde_json::to_value(&a).unwrap_or_default());
        }
    }
}

/// worker.rs, before posting a batch: where this recording lives on this computer.
pub fn note_media(media_id: &str, source_id: &str, resolved_url: &str) {
    if let Some(n) = global() {
        n.shard.note_media(media_id, source_id, resolved_url);
    }
}

pub enum Post {
    Ok,
    Stale(ApiError),
    Permanent(ApiError),
    Unreachable(String),
}

/// worker.rs `try_once`: `if let Some(p) = shard::node::commit_and_ack(...) { ... }`. Commit locally, then give the head
/// the receipt (lease check, checkpoint, thumbnail, owner). The local commit comes first: a checkpoint never gets ahead
/// of durable rows. Replaying a batch is harmless (content-derived ids).
pub fn commit_and_ack(batch: &IndexBatch, agent: &ureq::Agent, base: &str) -> Option<Post> {
    let node = global()?;
    let mut b = batch.clone();
    let hex = b.thumbnail_hex.take().filter(|h| !h.is_empty());
    if hex.is_some() {
        b.observation.thumbnail = Some(format!("{}.jpg", b.observation.observation_id));
    }
    let out = match node.shard.commit(&b) {
        Ok(o) => o,
        Err(e) if e.code == "invalid_request" => return Some(Post::Permanent(e)),
        Err(e) => return Some(Post::Unreachable(format!("local shard: {}", e.message))),
    };
    if !out.duplicate {
        let o = &b.observation;
        node.shard.push_event("index", &serde_json::json!({ "media_id": o.media_id, "media_time_us": o.media_time_us, "detections": b.detections.len(), "snapshot": out.snapshot, "node": node.id }));
        for a in node.shard.evaluate(o) {
            node.shard.push_event("alert", &serde_json::to_value(&a).unwrap_or_default());
        }
    }
    let receipt = relay::Receipt {
        node_id: node.id.clone(),
        lease: b.lease.clone(),
        detections: b.detections.iter().map(|d| serde_json::json!({ "class": d.class, "confidence": d.confidence, "bbox": d.bbox, "track_id": d.track_id, "color": d.color })).collect(),
        observation: b.observation,
        thumbnail_hex: hex,
        snapshot: out.snapshot,
    };
    let mut req = agent.post(&format!("{base}/api/v1/shard/ack"));
    if let Ok(t) = std::env::var("CW_WORKER_TOKEN") {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    Some(match req.send_json(&receipt) {
        Ok(_) => Post::Ok,
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            let e = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| serde_json::from_value::<ApiError>(v.get("error").cloned()?).ok())
                .unwrap_or_else(|| ApiError::new("invalid_request", format!("HTTP {code}")));
            if e.code == "stale_lease" {
                Post::Stale(e)
            } else if (400..500).contains(&code) && !matches!(code, 408 | 429) {
                Post::Permanent(e)
            } else {
                Post::Unreachable(format!("head answered HTTP {code}: {}", e.message))
            }
        }
        Err(ureq::Error::Transport(t)) => Post::Unreachable(format!("head unreachable: {t}")),
    })
}

fn respond(req: tiny_http::Request, r: api::Reply) {
    let mut resp = tiny_http::Response::from_data(r.body).with_status_code(tiny_http::StatusCode(r.status));
    if let Ok(h) = tiny_http::Header::from_bytes(b"Content-Type", r.ctype.as_bytes()) {
        resp = resp.with_header(h);
    }
    for (k, v) in r.headers {
        if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            resp = resp.with_header(h);
        }
    }
    let _ = req.respond(resp);
}

fn serve_one(node: &Arc<Node>, mut req: tiny_http::Request) {
    let method = req.method().as_str().to_string();
    let url = req.url().to_string();
    // Worker credential only: the bearer this node announced over its authenticated heartbeat. Constant-time compare.
    let tok = req.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.as_str().trim_start_matches("Bearer ").trim().to_string()).unwrap_or_default();
    if !crate::watch::auth::ct_eq(tok.as_bytes(), node.secret.as_bytes()) {
        return respond(req, api::Reply::err(ApiError::new("unauthorized", "shard endpoints accept the head's worker credential only")));
    }
    let range = req.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.as_str().to_string());
    let mut body = Vec::new();
    let _ = req.as_reader().take(2_000_000).read_to_end(&mut body);
    let r = handle(node, &method, &url, &body, range.as_deref()).unwrap_or_else(|e| api::Reply::err(e));
    respond(req, r);
}

fn handle(node: &Arc<Node>, method: &str, url: &str, body: &[u8], range: Option<&str>) -> StoreResult<api::Reply> {
    let path = url.split('?').next().unwrap_or(url);
    let rest = path.strip_prefix("/api/v1/shard/").ok_or_else(|| ApiError::new("not_found", "unknown shard route"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    let sh = &node.shard;
    let d = Duration::ZERO;
    let req = || serde_json::from_slice::<ShardReq>(body).map_err(|e| ApiError::new("invalid_request", format!("invalid JSON: {e}")));
    Ok(match (method, parts.as_slice()) {
        ("GET", ["status"]) => api::Reply::json(&sh.status(d)?),
        ("POST", ["detections"]) => api::Reply::json(&sh.page(Kind::Detections, &req()?, d)?),
        ("POST", ["summaries"]) => api::Reply::json(&sh.page(Kind::Summaries, &req()?, d)?),
        ("POST", ["relations"]) => api::Reply::json(&sh.page(Kind::Relations, &req()?, d)?),
        ("POST", ["count"]) => api::Reply::json(&serde_json::json!({ "count": sh.count(&req()?, d)? })),
        ("POST", ["observations"]) => {
            let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| ApiError::new("invalid_request", format!("invalid JSON: {e}")))?;
            let (m, a, b) = (v["media_id"].as_str().unwrap_or(""), v["from_us"].as_i64().unwrap_or(0), v["to_us"].as_i64().unwrap_or(0));
            if b < a || b - a > 600_000_000 {
                return Err(ApiError::new("invalid_request", "observation window must be 0..600 s"));
            }
            api::Reply::json(&sh.observations(m, a, b, d)?)
        }
        // Original video, frames, thumbnails, sprites: the node's own `/api/v1/media/*` handlers over its shard catalog.
        ("GET", ["media", ..]) => {
            let api = node.api.get_or_init(|| Arc::new(api::Api::new(sh.store.clone(), data_dir(), crate::watch::auth::Auth::load(&crate::watch::repo_root()))));
            let inner = url.replacen("/api/v1/shard/media/", "/api/v1/media/", 1);
            api::handle(api, "GET", &inner, &[], range).ok_or_else(|| ApiError::new("not_found", "unknown media route"))?
        }
        _ => return Err(ApiError::new("not_found", "unknown shard route")),
    })
}
