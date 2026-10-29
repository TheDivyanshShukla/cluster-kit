//! Shared, versioned contracts. Every module and every `/api/v1` payload uses these types.
//!
//! Time is integer microseconds. `media_time_us` is always known for video; capture UTC is
//! optional and stays `None` when unknown. Indexing wall-clock is never presented as capture time.
//! IDs are opaque strings. Observation and detection IDs are derived from content, not from
//! processing time, so a retried batch produces the same keys and cannot raise counts.
//!
//! Changing anything here is a coordinator decision (docs/VISION_AGENT_TASKS.md).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u32 = 1;

pub type Us = i64;

/// Stable hex ID from parts. Same parts, same ID, on every machine.
pub fn stable_id(prefix: &str, parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    let d = h.finalize();
    let hex: String = d[..12].iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}_{hex}")
}

/// Observation key: media fingerprint + frame position + analysis version.
pub fn observation_id(media_fingerprint: &str, media_time_us: Us, analysis_version: &str) -> String {
    stable_id("obs", &[media_fingerprint, &media_time_us.to_string(), analysis_version])
}

/// Detection key: observation + model + ordinal within that model's output for the frame.
pub fn detection_id(observation_id: &str, model_version_id: &str, ordinal: usize) -> String {
    stable_id("det", &[observation_id, model_version_id, &ordinal.to_string()])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Live,
    Webcam,
    Archive,
    Image,
    Collection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    /// Path or URL with credentials removed. Credentials live behind `credential_ref`.
    pub location: String,
    #[serde(default)]
    pub credential_ref: Option<String>,
    /// Worker IDs allowed to run this source. Empty = any. A webcam names exactly one.
    #[serde(default)]
    pub eligible_workers: Vec<String>,
    pub enabled: bool,
    #[serde(default)]
    pub capture_timezone: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Missing,
    Expired,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyStatus {
    None,
    Pending,
    Ready,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: String,
    pub source_id: String,
    pub uri: String,
    /// Content fingerprint (sha256 of size + first/last MiB is acceptable; record method).
    pub fingerprint: String,
    #[serde(default)]
    pub duration_us: Option<Us>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub codec: Option<String>,
    /// e.g. "1/90000"
    #[serde(default)]
    pub timebase: Option<String>,
    /// Real capture start in UTC microseconds when known. Never guessed from file names.
    #[serde(default)]
    pub capture_start_utc_us: Option<Us>,
    pub proxy: ProxyStatus,
    pub availability: Availability,
    /// True for a still image; review shows it on a contact sheet, not a fake-duration strip.
    #[serde(default)]
    pub still: bool,
    /// Computer that ingested this recording and holds its observations (`CW_SHARDS=1`). `None` = the head.
    #[serde(default)]
    pub node_id: Option<String>,
}

impl MediaAsset {
    /// Recordings without known capture time each get their own clock domain.
    pub fn clock_domain(&self) -> ClockDomain {
        match self.capture_start_utc_us {
            Some(_) => ClockDomain::Utc,
            None => ClockDomain::Media { media_id: self.id.clone() },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClockDomain {
    Utc,
    Media { media_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    Object,
    Tracker,
    Relation,
    Verifier,
    Fight,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelVersion {
    pub id: String,
    pub version: String,
    pub artifact_hash: String,
    pub adapter: AdapterKind,
    /// e.g. "yolov5", "yolov8", "yolov8-single"
    pub output_schema: String,
    /// Index i of the model output is labels[i]. Must match the artifact, not a subset.
    pub labels: Vec<String>,
    #[serde(default)]
    pub supported_relations: Vec<Predicate>,
    /// Per-class confidence thresholds; "*" is the default.
    #[serde(default)]
    pub thresholds: BTreeMap<String, f32>,
    /// Effective provider actually in use, e.g. "cpu", "coreml", "cuda".
    pub provider: String,
    #[serde(default)]
    pub input_size: Option<u32>,
    /// "unvalidated" until a labeled evaluation exists. Never a made-up accuracy.
    pub validation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Frame accounting. "Completed" never means every frame was inferred; this says how many were.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub decoded: u64,
    pub inferred: u64,
    pub skipped: u64,
    pub failed: u64,
    /// Media-time intervals actually analyzed, [start_us, end_us).
    #[serde(default)]
    pub analyzed: Vec<[Us; 2]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IndexJob {
    pub id: String,
    pub manifest_version: String,
    pub media_ids: Vec<String>,
    pub model_versions: Vec<String>,
    /// Free-form sampling settings (gate switches, every-N, width).
    pub sampling: serde_json::Value,
    pub state: JobState,
    /// Last media time durably committed per media ID. Advances only after commit.
    #[serde(default)]
    pub checkpoint: BTreeMap<String, Us>,
    #[serde(default)]
    pub coverage: Coverage,
    #[serde(default)]
    pub error: Option<ApiError>,
    pub created_utc_us: Us,
    #[serde(default)]
    pub finished_utc_us: Option<Us>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskLease {
    pub task_id: String,
    pub job_id: String,
    pub media_id: String,
    /// [start_us, end_us) of media time; end None = to end of media / live.
    pub segment: [Option<Us>; 2],
    pub worker: String,
    /// Bumped on every reassignment. A batch from an older generation is rejected.
    pub generation: u64,
    pub expires_utc_us: Us,
    pub attempt: u32,
    /// Overlap read before `segment[0]` for tracker warm-up. Observations inside it are not indexed.
    #[serde(default)]
    pub overlap_us: Us,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    #[default]
    Ok,
    Blurry,
    Blank,
    Partial,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameObservation {
    pub observation_id: String,
    pub source_id: String,
    pub media_id: String,
    pub job_id: String,
    #[serde(default)]
    pub frame_index: Option<u64>,
    #[serde(default)]
    pub pts: Option<i64>,
    #[serde(default)]
    pub timebase: Option<String>,
    pub media_time_us: Us,
    #[serde(default)]
    pub capture_time_utc_us: Option<Us>,
    pub indexed_at_utc_us: Us,
    pub model_versions: Vec<String>,
    #[serde(default)]
    pub quality: Quality,
    #[serde(default)]
    pub thumbnail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub detection_id: String,
    pub observation_id: String,
    pub class: String,
    pub confidence: f32,
    /// Normalized [x0, y0, x1, y1] in 0..1 of the analyzed frame.
    pub bbox: [f32; 4],
    pub model_version: String,
    /// Scoped to (source, media, job). Never a cross-camera identity.
    #[serde(default)]
    pub track_id: Option<String>,
    /// "model" | "fused:<ids>" — which modules produced it and whether it was deduplicated.
    #[serde(default = "default_provenance")]
    pub provenance: String,
    #[serde(default)]
    pub color: Option<String>,
}

fn default_provenance() -> String {
    "model".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    /// Spatial proximity heuristic. Approximate; does not prove holding.
    Near,
    /// Requires a relation module that declares it.
    Holding,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    pub relation_id: String,
    pub observation_id: String,
    pub subject_detection_id: String,
    #[serde(default)]
    pub subject_track_id: Option<String>,
    pub predicate: Predicate,
    pub object_detection_id: String,
    #[serde(default)]
    pub object_track_id: Option<String>,
    pub object_class: String,
    pub confidence: f32,
    pub module_version: String,
    /// e.g. "bbox-proximity", "pose-hand-overlap"
    pub method: String,
}

/// Whether installed modules can answer a question at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Supported,
    Unsupported,
    Unknown,
}

/// A count that may be unknown. Unknown is not zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Count {
    Known(u32),
    Unknown,
    Unsupported,
}

impl Count {
    pub fn known(self) -> Option<u32> {
        match self {
            Count::Known(n) => Some(n),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameSummary {
    pub observation_id: String,
    pub source_id: String,
    pub media_id: String,
    pub media_time_us: Us,
    #[serde(default)]
    pub capture_time_utc_us: Option<Us>,
    #[serde(default)]
    pub zone: Option<String>,
    pub min_confidence: f32,
    /// Distinct objects per class in this frame (after fusion), keyed by class.
    pub class_counts: BTreeMap<String, Count>,
    /// Distinct subjects per "predicate:object_class", e.g. "holding:cell phone".
    pub relation_counts: BTreeMap<String, Count>,
    pub quality: Quality,
}

impl FrameSummary {
    /// Missing class means the frame was not analyzed for it: unknown, not zero.
    pub fn class(&self, class: &str) -> Count {
        self.class_counts.get(class).copied().unwrap_or(Count::Unknown)
    }
    pub fn relation(&self, predicate: Predicate, object_class: &str) -> Count {
        self.relation_counts.get(&relation_key(predicate, object_class)).copied().unwrap_or(Count::Unsupported)
    }
}

pub fn relation_key(predicate: Predicate, object_class: &str) -> String {
    let p = match predicate {
        Predicate::Near => "near",
        Predicate::Holding => "holding",
    };
    format!("{p}:{object_class}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    Pending,
    Active,
    Resolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Unreviewed,
    Confirmed,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub rule_id: String,
    pub rule_version: String,
    pub source_id: String,
    #[serde(default)]
    pub media_id: Option<String>,
    pub start_us: Us,
    #[serde(default)]
    pub end_us: Option<Us>,
    pub clock: ClockDomain,
    pub values: serde_json::Value,
    pub severity: String,
    pub evidence: Vec<String>,
    pub state: AlertState,
    pub decision: Decision,
    #[serde(default)]
    pub actor: Option<String>,
    pub updated_utc_us: Us,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryScope {
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub media_ids: Vec<String>,
    /// Applies to capture UTC when known; media-relative otherwise is rejected (use media scope).
    #[serde(default)]
    pub from_utc_us: Option<Us>,
    #[serde(default)]
    pub to_utc_us: Option<Us>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    Detections,
    Tracks,
    Relations,
    Frames,
    Events,
    Aggregate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryRun {
    pub id: String,
    pub schema_version: u32,
    pub normalized: String,
    pub scope: QueryScope,
    pub kind: ResultKind,
    /// Highest committed index sequence visible to this run.
    pub snapshot: u64,
    pub coverage: Coverage,
    pub warnings: Vec<String>,
    pub result_count: u64,
    pub elapsed_us: u64,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    Unreviewed,
    Reviewed,
    Confirmed,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewMatch {
    pub id: String,
    pub query_run_id: String,
    pub source_id: String,
    pub media_id: String,
    pub clock: ClockDomain,
    /// Exact matched interval in media time.
    pub start_us: Us,
    pub end_us: Us,
    /// Context interval, clamped to media bounds. Always contains [start_us, end_us].
    pub context_start_us: Us,
    pub context_end_us: Us,
    #[serde(default)]
    pub capture_start_utc_us: Option<Us>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    pub observation_ids: Vec<String>,
    pub detection_ids: Vec<String>,
    #[serde(default)]
    pub relation_ids: Vec<String>,
    /// Distinct track IDs inside the window. Five observations of one track are one.
    #[serde(default)]
    pub distinct_tracks: u32,
    pub max_confidence: f32,
    pub verification: Verification,
    #[serde(default)]
    pub still: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectionItem {
    pub match_id: String,
    pub in_us: Us,
    pub out_us: Us,
    #[serde(default)]
    pub note: String,
    pub verification: Verification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportStatus {
    None,
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewSelection {
    pub id: String,
    /// Bumped on every PATCH; a PATCH with a stale version is rejected.
    pub version: u64,
    pub title: String,
    pub items: Vec<SelectionItem>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub actor: Option<String>,
    pub export: ExportStatus,
    pub updated_utc_us: Us,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: String,
    pub utc_us: Us,
    pub actor: String,
    pub action: String,
    pub targets: Vec<String>,
    pub result: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    /// Sanitized: no credentials, no full RTSP URLs with passwords.
    pub detail: String,
}

/// Structured error envelope for every `/api/v1` failure: `{"error": ApiError}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    /// Stable machine code, e.g. "invalid_request", "unsupported", "not_found", "conflict".
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub correlation_id: Option<String>,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        ApiError { code: code.into(), message: message.into(), retryable: false, correlation_id: None }
    }
    pub fn status(&self) -> u16 {
        match self.code.as_str() {
            "invalid_request" | "unsupported" | "parse_error" => 400,
            "unauthorized" => 401,
            "forbidden" => 403,
            "not_found" => 404,
            "conflict" | "stale_lease" | "retry_budget_exhausted" => 409,
            "too_large" => 413,
            "unavailable" | "model_unavailable" => 503,
            _ => 500,
        }
    }
}

/// One frame's worth of analysis sent from a worker to the head. Idempotent by observation ID.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IndexBatch {
    pub schema_version: u32,
    #[serde(default)]
    pub lease: Option<LeaseRef>,
    pub observation: FrameObservation,
    pub detections: Vec<Detection>,
    #[serde(default)]
    pub relations: Vec<Relation>,
    /// Hex JPEG of the exact inference frame.
    #[serde(default)]
    pub thumbnail_hex: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LeaseRef {
    pub task_id: String,
    pub generation: u64,
}

/// Filter pushed down to storage. All set fields AND together.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DetectionFilter {
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub media_ids: Vec<String>,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub min_confidence: Option<f32>,
    #[serde(default)]
    pub model_versions: Vec<String>,
    #[serde(default)]
    pub media_from_us: Option<Us>,
    #[serde(default)]
    pub media_to_us: Option<Us>,
    #[serde(default)]
    pub utc_from_us: Option<Us>,
    #[serde(default)]
    pub utc_to_us: Option<Us>,
    #[serde(default)]
    pub track_ids: Vec<String>,
}

/// Stable cursor page: rows ordered by (media_id, media_time_us, id).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub after: Option<String>,
    pub limit: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchOutcome {
    pub inserted_observations: u32,
    pub inserted_detections: u32,
    pub inserted_relations: u32,
    /// True when the observation already existed; nothing changed.
    pub duplicate: bool,
    pub snapshot: u64,
}

pub type StoreResult<T> = Result<T, ApiError>;

/// Head-side metadata store. SQLite WAL first; PostgreSQL later behind the same trait.
pub trait Store: Send + Sync {
    fn upsert_source(&self, s: &Source) -> StoreResult<()>;
    fn sources(&self) -> StoreResult<Vec<Source>>;
    fn upsert_media(&self, m: &MediaAsset) -> StoreResult<()>;
    fn media(&self, id: &str) -> StoreResult<Option<MediaAsset>>;
    fn media_for_source(&self, source_id: &str) -> StoreResult<Vec<MediaAsset>>;
    fn upsert_model(&self, m: &ModelVersion) -> StoreResult<()>;
    fn models(&self) -> StoreResult<Vec<ModelVersion>>;
    fn upsert_job(&self, j: &IndexJob) -> StoreResult<()>;
    fn job(&self, id: &str) -> StoreResult<Option<IndexJob>>;
    fn jobs(&self) -> StoreResult<Vec<IndexJob>>;
    /// One transaction: observation + detections + relations + summary + checkpoint advance.
    fn commit_batch(&self, b: &IndexBatch) -> StoreResult<BatchOutcome>;
    fn detections(&self, f: &DetectionFilter, page: &Page) -> StoreResult<(Vec<(FrameObservation, Detection)>, Option<String>)>;
    fn count_detections(&self, f: &DetectionFilter) -> StoreResult<u64>;
    fn observations(&self, media_id: &str, from_us: Us, to_us: Us) -> StoreResult<Vec<(FrameObservation, Vec<Detection>, Vec<Relation>)>>;
    fn summaries(&self, f: &DetectionFilter, page: &Page) -> StoreResult<(Vec<FrameSummary>, Option<String>)>;
    fn relations(&self, f: &DetectionFilter, predicate: Option<Predicate>, page: &Page) -> StoreResult<(Vec<Relation>, Option<String>)>;
    fn save_query_run(&self, q: &QueryRun, matches: &[ReviewMatch]) -> StoreResult<()>;
    fn query_run(&self, id: &str) -> StoreResult<Option<QueryRun>>;
    fn matches(&self, query_run_id: &str, page: &Page) -> StoreResult<(Vec<ReviewMatch>, Option<String>)>;
    fn save_review(&self, r: &ReviewSelection, expected_version: Option<u64>) -> StoreResult<ReviewSelection>;
    fn review(&self, id: &str) -> StoreResult<Option<ReviewSelection>>;
    fn reviews(&self) -> StoreResult<Vec<ReviewSelection>>;
    fn upsert_alert(&self, a: &Alert) -> StoreResult<()>;
    fn alerts(&self, page: &Page) -> StoreResult<(Vec<Alert>, Option<String>)>;
    fn audit(&self, r: &AuditRecord) -> StoreResult<()>;
    fn audit_log(&self, text: Option<&str>, page: &Page) -> StoreResult<(Vec<AuditRecord>, Option<String>)>;
    /// Highest committed sequence. Query runs record it as their snapshot.
    fn snapshot(&self) -> StoreResult<u64>;
    /// One match by ID (review manifests). Default keeps older implementations compiling.
    fn review_match(&self, _id: &str) -> StoreResult<Option<ReviewMatch>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = observation_id("fp1", 1_000_000, "yolov5n@1");
        assert_eq!(a, observation_id("fp1", 1_000_000, "yolov5n@1"));
        assert_ne!(a, observation_id("fp1", 1_000_001, "yolov5n@1"));
        assert_ne!(a, observation_id("fp1", 1_000_000, "yolov5n@2"));
        assert!(a.starts_with("obs_"));
    }

    #[test]
    fn missing_class_is_unknown_not_zero() {
        let s: FrameSummary = serde_json::from_str(include_str!("contracts/frame_summary.v1.json")).unwrap();
        assert_eq!(s.class("person"), Count::Known(60));
        assert_eq!(s.class("knife"), Count::Unknown);
        assert_eq!(s.relation(Predicate::Holding, "cell phone"), Count::Known(12));
        assert_eq!(s.relation(Predicate::Holding, "book"), Count::Unsupported);
    }

    #[test]
    fn fixtures_parse() {
        let _: IndexBatch = serde_json::from_str(include_str!("contracts/index_batch.v1.json")).unwrap();
        let _: ReviewMatch = serde_json::from_str(include_str!("contracts/review_match.v1.json")).unwrap();
        let _: QueryRun = serde_json::from_str(include_str!("contracts/query_run.v1.json")).unwrap();
        let _: TaskLease = serde_json::from_str(include_str!("contracts/task_lease.v1.json")).unwrap();
        let _: ReviewSelection = serde_json::from_str(include_str!("contracts/review_selection.v1.json")).unwrap();
        let e: serde_json::Value = serde_json::from_str(include_str!("contracts/error.v1.json")).unwrap();
        let e: ApiError = serde_json::from_value(e["error"].clone()).unwrap();
        assert_eq!(e.status(), 400);
    }

    #[test]
    fn unknown_capture_time_gets_own_clock() {
        let m = MediaAsset {
            id: "m1".into(), source_id: "s".into(), uri: "x.mp4".into(), fingerprint: "f".into(),
            duration_us: Some(1), width: None, height: None, codec: None, timebase: None,
            capture_start_utc_us: None, proxy: ProxyStatus::None, availability: Availability::Available, still: false, node_id: None,
        };
        assert_eq!(m.clock_domain(), ClockDomain::Media { media_id: "m1".into() });
    }
}
