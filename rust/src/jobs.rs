//! Shared job types, the head context and the event bus. Frozen contract: only the lead edits this file.
//! JSON shapes here are the API contract (docs/API.md, console/src/lib/api/schema.ts).

use serde::{Deserialize, Serialize};
use std::sync::{mpsc, Arc, Mutex};

use crate::head::Shared;
use crate::store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Cancelled,
    Failed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Cancelled => "cancelled",
            JobState::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<JobState> {
        Some(match s {
            "queued" => JobState::Queued,
            "running" => JobState::Running,
            "done" => JobState::Done,
            "cancelled" => JobState::Cancelled,
            "failed" => JobState::Failed,
            _ => return None,
        })
    }
    pub fn finished(self) -> bool {
        matches!(self, JobState::Done | JobState::Cancelled | JobState::Failed)
    }
}

/// What to run. `kind` is a proto KIND_* value; `arg` is its wire argument (prefix nibbles, command line, ns u64 LE).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobSpec {
    pub name: String,
    pub kind: u8,
    pub from: u64,
    pub end: u64,
    /// 0 = automatic (head picks).
    pub chunk: u64,
    pub arg: Vec<u8>,
    /// Worker addresses allowed to take chunks; empty = all. Kept for old clients; prefer `selector`.
    pub only: Vec<String>,
    pub submitter: String,
    /// Which PCs may run it (select.rs language); "" = any (ORG-04).
    #[serde(default)]
    pub selector: String,
    /// 0 urgent, 1 high, 2 normal (default), 3 low, 4 background (SCH-02).
    #[serde(default = "normal_priority")]
    pub priority: u8,
    /// Chunk policy (SCH-06): "fixed" (the paper baseline), "guided", "adaptive"; "" = adaptive.
    #[serde(default)]
    pub policy: String,
    /// How the head combines results (JOB-07): "" (concat), "sum", "count", "sort", "lines", "top:N".
    #[serde(default)]
    pub reducer: String,
    /// List job (JOB-01): the items; then from = 0, end = items.len(), and tasks are item index ranges. Stored apart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
}

pub fn normal_priority() -> u8 {
    2
}

impl Default for JobSpec {
    fn default() -> JobSpec {
        JobSpec {
            name: String::new(),
            kind: 0,
            from: 0,
            end: 0,
            chunk: 0,
            arg: vec![],
            only: vec![],
            submitter: String::new(),
            selector: String::new(),
            priority: normal_priority(),
            policy: String::new(),
            reducer: String::new(),
            items: vec![],
        }
    }
}

pub const PRIORITIES: &[&str] = &["urgent", "high", "normal", "low", "background"];

/// A job as stored and as the API returns it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobRow {
    pub id: u64,
    pub name: String,
    /// "sha" | "cmd" | "synth"
    pub kind: String,
    pub from: u64,
    pub end: u64,
    pub chunk: u64,
    pub total: u64,
    /// Numbers completed (sum of stored part ranges).
    pub done: u64,
    pub state: JobState,
    pub submitter: String,
    pub created_ms: i64,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
    pub error: Option<String>,
    #[serde(default)]
    pub selector: String,
    #[serde(default = "normal_priority")]
    pub priority: u8,
    #[serde(default)]
    pub policy: String,
    #[serde(default)]
    pub reducer: String,
    /// Estimated finish (SCH-03), filled in by the API from the head's measured rates; None = not running or unknown.
    #[serde(default)]
    pub eta_ms: Option<i64>,
}

pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        crate::proto::KIND_SHA => "sha",
        crate::proto::KIND_SYNTH => "synth",
        crate::proto::KIND_LIST => "list",
        _ => "cmd",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    pub id: u64,
    pub ts_ms: i64,
    /// e.g. "job.submitted", "job.done", "job.cancelled", "node.joined", "node.lost", "user.login", "user.created"
    pub kind: String,
    pub actor: String,
    pub detail: String,
}

/// Power policy (NET-06), stored as JSON in store meta "power". mode "off": the head never sleeps PCs;
/// "eco": PCs idle for idle_min with no queued work go to sleep, keeping keep_awake awake; queued work wakes them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerSettings {
    pub mode: String,
    pub idle_min: u32,
    pub keep_awake: u32,
    pub wake_on_demand: bool,
}

impl Default for PowerSettings {
    fn default() -> Self {
        PowerSettings { mode: "off".into(), idle_min: 15, keep_awake: 4, wake_on_demand: true }
    }
}

/// Dynamic heads (HA-10), stored as JSON in store meta "ha_settings": the primary keeps target_standbys standbys,
/// recruiting connected PCs that match `selector` (different labs first) whenever one is missing, e.g. after a failover.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HaSettings {
    pub target_standbys: u32,
    pub selector: String,
}

impl Default for HaSettings {
    fn default() -> Self {
        HaSettings { target_standbys: 2, selector: "!virtual".into() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeRow {
    pub node_id: String,
    pub host: String,
    pub addr: String,
    /// proto::Inventory as JSON text
    pub inventory: String,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    /// One of NODE_STATES. New PCs start "pending" (FLT-04) and get no tasks until approved.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub state_reason: String,
    /// Who set the state (user name, or "head").
    #[serde(default)]
    pub state_by: String,
    /// Seat on a floor plan (ORG-01): lab id and grid cell, or none.
    #[serde(default)]
    pub lab_id: Option<u64>,
    #[serde(default)]
    pub x: Option<u32>,
    #[serde(default)]
    pub y: Option<u32>,
    /// Calibration (FLT-06): this PC's SHA-256 rate in MH/s from a 3 s benchmark at join, if measured.
    #[serde(default)]
    pub calib_mhs: Option<f64>,
}

/// ORG-09 node states. pending: joined, not approved. active: takes tasks. cordoned: no new tasks.
/// draining: finishes its tasks, then the head sets cordoned. maintenance: no tasks, shown as such. retired: forgotten PC.
pub const NODE_STATES: &[&str] = &["pending", "active", "cordoned", "draining", "maintenance", "retired"];

/// States in which a node may receive new tasks.
pub fn takes_tasks(state: &str) -> bool {
    state == "active"
}

/// Named group of nodes (ORG-02). Members are node ids; a node may be in several pools.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PoolRow {
    pub name: String,
    pub members: Vec<String>,
    pub note: String,
}

/// Upper limit on what the cluster uses on the nodes matching `selector` (ORG-05). Several caps on a node: the minimum wins.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CapRow {
    pub id: u64,
    pub selector: String,
    pub max_threads: Option<u32>,
    pub note: String,
}

/// Capacity the scheduler must leave free among the nodes matching `selector` (ORG-06): max(keep_n, keep_pct %).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReserveRow {
    pub id: u64,
    pub selector: String,
    pub keep_n: u32,
    pub keep_pct: u32,
    pub note: String,
}

/// Weekly time rule (ORG-07): on `days` between `start` and `end` (local "HH:MM", CK_TZ_OFFSET_MIN, default 330 = IST),
/// the PCs matching `selector` take no jobs (`action` "block") or at most `cap_threads` threads ("cap").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowRow {
    pub id: u64,
    pub selector: String,
    /// e.g. "mon-fri", "sat,sun", "mon,wed,fri"
    pub days: String,
    pub start: String,
    pub end: String,
    pub action: String,
    pub cap_threads: Option<u32>,
    pub note: String,
}

/// Dated booking (ORG-08): between start and end the PCs matching `selector` take no jobs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReservationRow {
    pub id: u64,
    pub selector: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub note: String,
}

/// A lab (room) with a grid floor plan of w x h cells (ORG-01, UI-02).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LabRow {
    pub id: u64,
    pub name: String,
    pub w: u32,
    pub h: u32,
    pub created_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserRow {
    pub name: String,
    /// "viewer" | "researcher" | "operator" | "admin"
    pub role: String,
    /// PBKDF2 record, never serialised to the API
    #[serde(skip_serializing, default)]
    pub pw_hash: String,
    pub created_ms: i64,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Server-sent events fan-out: `publish(event, json)` reaches every live subscriber; dead ones are dropped.
#[derive(Default)]
pub struct Bus(Mutex<Vec<mpsc::Sender<(String, String)>>>);

impl Bus {
    pub fn publish(&self, event: &str, json: &str) {
        self.0.lock().unwrap().retain(|s| s.send((event.to_string(), json.to_string())).is_ok());
    }
    pub fn subscribe(&self) -> mpsc::Receiver<(String, String)> {
        let (tx, rx) = mpsc::channel();
        self.0.lock().unwrap().push(tx);
        rx
    }
}

/// Everything the head's network, API and scheduler code share.
pub struct Ctx {
    pub sh: Shared,
    pub store: Arc<Store>,
    pub bus: Bus,
    /// High availability state (repl.rs).
    pub repl: crate::repl::Repl,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names_and_bus() {
        for s in [JobState::Queued, JobState::Running, JobState::Done, JobState::Cancelled, JobState::Failed] {
            assert_eq!(JobState::parse(s.as_str()), Some(s));
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.as_str()));
        }
        let bus = Bus::default();
        let rx = bus.subscribe();
        drop(bus.subscribe());
        bus.publish("job", "{}");
        assert_eq!(rx.recv().unwrap(), ("job".into(), "{}".into()));
        assert_eq!(bus.0.lock().unwrap().len(), 1, "dead subscriber dropped");
        assert_eq!(JobSpec::default().priority, 2, "default priority is normal, not urgent");
        let u = UserRow { name: "a".into(), role: "admin".into(), pw_hash: "secret".into(), created_ms: 1 };
        assert!(!serde_json::to_string(&u).unwrap().contains("secret"));
    }
}
