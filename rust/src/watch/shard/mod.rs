//! Distributed search (docs/DISTRIBUTED.md). Opt-in with `CW_SHARDS=1`; with it off nothing here runs.
//!
//! Recordings are sharded by the computer that ingested them. Each node keeps its observations in its own SQLite
//! file and answers reads over `/api/v1/shard/*`. The head keeps the catalog and human state, and reads through
//! `FederatedStore`, which fans out to the nodes, merges in (media, time, id) order and reports who answered.

pub mod federated;
pub mod merge;
pub mod node;
pub mod relay;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::watch::api::{Api, Reply};
use crate::watch::domain::*;

pub fn enabled() -> bool {
    std::env::var("CW_SHARDS").is_ok_and(|v| v == "1")
}

pub fn data_dir() -> PathBuf {
    crate::watch::repo_root().join("vision/data")
}

pub fn now_s() -> f64 {
    crate::watch::now_secs()
}

/// Per node call deadline (`CW_SHARD_DEADLINE_MS`, default 4000).
pub fn deadline() -> Duration {
    Duration::from_millis(std::env::var("CW_SHARD_DEADLINE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(4000))
}

// ------------------------------------------------------------------ wire types

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Detections,
    Summaries,
    Relations,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShardReq {
    #[serde(default)]
    pub filter: DetectionFilter,
    #[serde(default)]
    pub page: Page,
    /// Read at or below this commit sequence of the node. `None` = everything committed.
    #[serde(default)]
    pub snapshot: Option<u64>,
    #[serde(default)]
    pub predicate: Option<Predicate>,
}

/// One result row with its merge key (media_id, media_time_us, id).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub k: (String, i64, String),
    pub v: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Rows {
    pub rows: Vec<Row>,
    pub next: Option<String>,
    pub snapshot: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeStatus {
    pub node_id: String,
    pub snapshot: u64,
    pub media: Vec<String>,
    pub observations: u64,
}

pub type ObsRow = (FrameObservation, Vec<Detection>, Vec<Relation>);

/// A computer that can answer shard reads: in process (`LocalShard`) or over HTTP (`node::HttpClient`).
pub trait NodeClient: Send + Sync {
    fn page(&self, kind: Kind, r: &ShardReq, d: Duration) -> StoreResult<Rows>;
    fn observations(&self, media_id: &str, from_us: Us, to_us: Us, d: Duration) -> StoreResult<Vec<ObsRow>>;
    fn count(&self, r: &ShardReq, d: Duration) -> StoreResult<u64>;
    fn status(&self, d: Duration) -> StoreResult<NodeStatus>;
}

/// Sent by a worker in its heartbeat so the head can reach its shard.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Announce {
    pub node_id: String,
    pub url: String,
    /// Bearer the head presents to this node. Sent over the worker-authenticated heartbeat; never a user session.
    pub secret: String,
}

// ------------------------------------------------------------------ registry

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ok,
    Slow,
    Down,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Slow => "slow",
            State::Down => "down",
        }
    }
}

#[derive(Clone)]
pub struct NodeRec {
    pub id: String,
    pub url: String,
    secret: String,
    pub last_seen: f64,
    pub snapshot: u64,
    pub media: BTreeSet<String>,
    pub observations: u64,
    client: Option<Arc<dyn NodeClient>>,
    local: bool,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    id: String,
    url: String,
    snapshot: u64,
    media: BTreeSet<String>,
    observations: u64,
}

/// Seconds without a sign of life before a node is `slow` / `down`. A lease is 15 s, so `down` matches failover.
const SLOW_S: f64 = 6.0;
const DOWN_S: f64 = 15.0;

#[derive(Default)]
pub struct Registry {
    nodes: Mutex<BTreeMap<String, NodeRec>>,
    file: Mutex<Option<PathBuf>>,
    saved_at: Mutex<f64>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Known nodes survive a head restart (as `down` until they announce again), so a query right after a restart
    /// reports them missing instead of answering from nothing.
    pub fn load(&self, file: &Path) {
        *self.file.lock().unwrap() = Some(file.to_path_buf());
        let saved: Vec<Saved> = std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut g = self.nodes.lock().unwrap();
        for s in saved {
            g.entry(s.id.clone()).or_insert(NodeRec { id: s.id, url: s.url, secret: String::new(), last_seen: 0.0, snapshot: s.snapshot, media: s.media, observations: s.observations, client: None, local: false });
        }
    }

    fn save(&self, force: bool) {
        let Some(f) = self.file.lock().unwrap().clone() else { return };
        let mut at = self.saved_at.lock().unwrap();
        if !force && now_s() - *at < 2.0 {
            return;
        }
        *at = now_s();
        let list: Vec<Saved> = self.nodes.lock().unwrap().values().filter(|n| !n.local).map(|n| Saved { id: n.id.clone(), url: n.url.clone(), snapshot: n.snapshot, media: n.media.clone(), observations: n.observations }).collect();
        if let Ok(b) = serde_json::to_vec(&list) {
            let _ = std::fs::write(f, b);
        }
    }

    /// A node answers in process (the head's own catalog, tests).
    pub fn add_local(&self, id: &str, c: Arc<dyn NodeClient>) {
        self.nodes.lock().unwrap().insert(id.into(), NodeRec { id: id.into(), url: String::new(), secret: String::new(), last_seen: now_s(), snapshot: 0, media: BTreeSet::new(), observations: 0, client: Some(c), local: true });
    }

    pub fn announce(&self, a: &Announce) {
        if a.node_id.is_empty() || a.url.is_empty() || a.secret.is_empty() {
            return;
        }
        let changed = {
            let mut g = self.nodes.lock().unwrap();
            let n = g.entry(a.node_id.clone()).or_insert(NodeRec { id: a.node_id.clone(), url: a.url.clone(), secret: a.secret.clone(), last_seen: 0.0, snapshot: 0, media: BTreeSet::new(), observations: 0, client: None, local: false });
            let changed = n.url != a.url || n.secret != a.secret || n.client.is_none();
            n.url = a.url.clone();
            n.secret = a.secret.clone();
            n.last_seen = now_s();
            if changed {
                n.client = Some(Arc::new(node::HttpClient::new(&a.url, &a.secret)));
            }
            changed
        };
        self.save(changed);
    }

    pub fn note_media(&self, node: &str, media: &str) {
        if let Some(n) = self.nodes.lock().unwrap().get_mut(node) {
            n.media.insert(media.to_string());
        }
    }

    pub fn owner_of(&self, media: &str) -> Option<String> {
        self.nodes.lock().unwrap().values().filter(|n| !n.local && n.media.contains(media)).max_by(|a, b| a.last_seen.total_cmp(&b.last_seen)).map(|n| n.id.clone())
    }

    pub fn ids(&self) -> Vec<String> {
        self.nodes.lock().unwrap().keys().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<NodeRec> {
        self.nodes.lock().unwrap().get(id).cloned()
    }

    pub fn client(&self, id: &str) -> Option<Arc<dyn NodeClient>> {
        self.nodes.lock().unwrap().get(id).and_then(|n| n.client.clone())
    }

    pub fn state_of(n: &NodeRec) -> State {
        if n.client.is_none() {
            return State::Down;
        }
        if n.local {
            return State::Ok;
        }
        match now_s() - n.last_seen {
            a if a <= SLOW_S => State::Ok,
            a if a <= DOWN_S => State::Slow,
            _ => State::Down,
        }
    }

    pub fn state(&self, id: &str) -> State {
        self.get(id).map(|n| Self::state_of(&n)).unwrap_or(State::Down)
    }

    pub fn update_status(&self, id: &str, s: &NodeStatus) {
        if let Some(n) = self.nodes.lock().unwrap().get_mut(id) {
            n.last_seen = now_s();
            n.snapshot = s.snapshot;
            n.observations = s.observations;
            if !s.media.is_empty() {
                n.media = s.media.iter().cloned().collect();
            }
        }
        self.save(false);
    }

    pub fn list(&self) -> Vec<serde_json::Value> {
        self.nodes
            .lock()
            .unwrap()
            .values()
            .filter(|n| !(n.local && n.observations == 0))
            .map(|n| {
                serde_json::json!({
                    "id": n.id, "url": n.url, "state": Self::state_of(n).name(), "snapshot": n.snapshot,
                    "media": n.media.len(), "observations": n.observations,
                    "last_seen_s_ago": if n.last_seen > 0.0 { Some((now_s() - n.last_seen).max(0.0)) } else { None },
                })
            })
            .collect()
    }

    /// Poll every remote node's `/shard/status` so media sets and commit sequences stay current.
    pub fn spawn_poller(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(1500));
            for id in me.ids() {
                let Some(n) = me.get(&id) else { continue };
                let Some(c) = n.client.clone() else { continue };
                let local = n.local;
                if let Ok(s) = c.status(Duration::from_millis(1200)) {
                    if local {
                        if let Some(n) = me.nodes.lock().unwrap().get_mut(&id) {
                            n.snapshot = s.snapshot;
                            n.observations = s.observations;
                        }
                    } else {
                        me.update_status(&id, &s);
                    }
                }
            }
        });
    }
}

static REGISTRY: OnceLock<Arc<Registry>> = OnceLock::new();

pub fn registry() -> &'static Arc<Registry> {
    REGISTRY.get_or_init(|| Arc::new(Registry::new()))
}

// ------------------------------------------------------------------ per-run bookkeeping

#[derive(Clone, Debug, Default)]
pub struct NodeStat {
    pub rows: u64,
    pub elapsed_ms: u64,
    pub max_call_ms: u64,
    pub error: Option<String>,
    pub registry_state: Option<State>,
}

/// What one query run learned about the nodes. Thread-local: `FederatedStore::snapshot` starts a run on the
/// calling thread and every read on that thread adds to it.
#[derive(Clone, Debug, Default)]
pub struct RunCtx {
    pub vector: BTreeMap<String, u64>,
    pub stats: BTreeMap<String, NodeStat>,
    pub failed: BTreeSet<String>,
}

thread_local! {
    static CTX: RefCell<RunCtx> = RefCell::new(RunCtx::default());
}

pub(crate) fn with_ctx<R>(f: impl FnOnce(&mut RunCtx) -> R) -> R {
    CTX.with(|c| f(&mut c.borrow_mut()))
}

/// Take (and clear) the node report of the run on this thread.
pub fn take_run() -> RunCtx {
    CTX.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

#[derive(Clone, Debug)]
pub struct RunReport {
    pub nodes: Vec<serde_json::Value>,
    pub partial: bool,
    pub answered: usize,
    pub total: usize,
    pub vector: BTreeMap<String, u64>,
}

impl RunCtx {
    pub fn report(&self) -> Option<RunReport> {
        // The head's own catalog only counts when it holds observations (legacy workers, mixed deployments).
        let rows: Vec<(&String, &NodeStat)> = self.stats.iter().filter(|(id, s)| !(id.as_str() == "head" && s.rows == 0 && !self.failed.contains(*id))).collect();
        if rows.is_empty() {
            return None;
        }
        let dl = deadline().as_millis() as u64;
        let nodes: Vec<_> = rows
            .iter()
            .map(|(id, s)| {
                let state = if self.failed.contains(*id) {
                    "down"
                } else if s.max_call_ms > dl / 2 || s.registry_state == Some(State::Slow) {
                    "slow"
                } else {
                    "ok"
                };
                serde_json::json!({ "id": id, "state": state, "rows": s.rows, "elapsed_ms": s.elapsed_ms, "error": s.error })
            })
            .collect();
        let total = rows.len();
        let answered = rows.iter().filter(|(id, _)| !self.failed.contains(*id)).count();
        Some(RunReport { nodes, partial: answered < total, answered, total, vector: self.vector.clone() })
    }
}

/// Called by `POST /api/v1/queries` after a run. Adds the "N of M computers answered" warning to the stored run.
pub fn finish_run(store: &dyn Store, run: &mut QueryRun, matches: &[ReviewMatch]) -> Option<RunReport> {
    let rep = take_run().report()?;
    if rep.partial {
        run.warnings.push(format!("{} of {} computers answered; results may be incomplete", rep.answered, rep.total));
        let _ = store.save_query_run(run, matches);
    }
    federated::remember_vector(run, &rep.vector);
    Some(rep)
}

/// Adds `nodes` and `partial` to the `/api/v1/queries` response without touching existing fields.
pub fn decorate(v: &mut serde_json::Value, rep: Option<&RunReport>) {
    if let (Some(r), Some(o)) = (rep, v.as_object_mut()) {
        o.insert("nodes".into(), serde_json::Value::Array(r.nodes.clone()));
        o.insert("partial".into(), r.partial.into());
        o.insert("snapshot_vector".into(), serde_json::to_value(&r.vector).unwrap_or_default());
    }
}

// ------------------------------------------------------------------ wiring

/// head.rs: `let store = shard::wrap_store(store, &data_path(""));`
pub fn wrap_store(store: Arc<dyn Store>, data: &Path) -> Arc<dyn Store> {
    if !enabled() {
        return store;
    }
    let reg = registry().clone();
    reg.load(&data.join("shard_nodes.json"));
    let cat = node::LocalShard::over("head", store.clone(), &data.join("watch.db"));
    reg.add_local("head", Arc::new(cat));
    reg.spawn_poller();
    relay::load_cursors(&data.join("shard_cursor.json"));
    crate::watch::log("shards: on (this head reads through the federated store)");
    Arc::new(federated::FederatedStore::new(store, reg, data.to_path_buf()))
}

/// api.rs `handle()`, first line: head-side shard routes and the media proxy.
pub fn route(api: &Arc<Api>, method: &str, url: &str, body: &[u8], range: Option<&str>) -> Option<Reply> {
    if !enabled() {
        return None;
    }
    let path = url.split('?').next().unwrap_or(url);
    let rest = path.strip_prefix("/api/v1/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let r = match (method, parts.as_slice()) {
        ("POST", ["shard", "ack"]) => relay::ack(api, body),
        ("POST", ["shard", "events"]) => relay::events(api, body),
        ("GET", ["nodes"]) => Ok(Reply::json(&serde_json::json!({ "nodes": registry().list(), "shards": true }))),
        ("POST", ["federated", kind @ ("detections" | "summaries" | "relations")]) => federated::browse(api, kind, body),
        ("GET", ["media", id, rest @ ..]) if !rest.is_empty() => return relay::proxy_media(api, id, rest, url, range),
        _ => return None,
    };
    Some(r.unwrap_or_else(Reply::err))
}

// ------------------------------------------------------------------ small helpers shared by the submodules

pub(crate) fn ie<E: std::fmt::Display>(e: E) -> ApiError {
    ApiError::new("internal", e.to_string())
}

pub(crate) fn unavailable(msg: impl Into<String>) -> ApiError {
    let mut e = ApiError::new("unavailable", msg);
    e.retryable = true;
    e
}
