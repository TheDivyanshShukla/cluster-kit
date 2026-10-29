//! SQLite implementation of `domain::Store`.
//!
//! Every entity is stored as its JSON `body` plus typed, indexed columns for the fields queries
//! filter on. One connection behind a mutex (WAL lets other processes read). Media rows hold URIs
//! only. `commit_batch` is one transaction and idempotent (`INSERT OR IGNORE` on content-derived IDs).

use crate::watch::domain::*;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCHEMA_V1: &str = r#"
CREATE TABLE sources(id TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE media(id TEXT PRIMARY KEY, source_id TEXT NOT NULL, availability TEXT NOT NULL, body TEXT NOT NULL);
CREATE INDEX media_source ON media(source_id);
CREATE TABLE model_versions(id TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE jobs(id TEXT PRIMARY KEY, state TEXT NOT NULL, created_utc_us INTEGER NOT NULL,
  checkpoint TEXT NOT NULL, coverage TEXT NOT NULL, body TEXT NOT NULL);
CREATE TABLE observations(observation_id TEXT PRIMARY KEY, source_id TEXT NOT NULL, media_id TEXT NOT NULL,
  job_id TEXT NOT NULL, media_time_us INTEGER NOT NULL, capture_time_utc_us INTEGER,
  indexed_at_utc_us INTEGER NOT NULL, body TEXT NOT NULL);
CREATE INDEX obs_media_time ON observations(media_id, media_time_us);
CREATE INDEX obs_source ON observations(source_id);
CREATE INDEX obs_utc ON observations(capture_time_utc_us);
CREATE TABLE detections(detection_id TEXT PRIMARY KEY,
  observation_id TEXT NOT NULL REFERENCES observations(observation_id),
  source_id TEXT NOT NULL, media_id TEXT NOT NULL, media_time_us INTEGER NOT NULL,
  capture_time_utc_us INTEGER, class TEXT NOT NULL, confidence REAL NOT NULL,
  model_version TEXT NOT NULL, track_id TEXT, body TEXT NOT NULL);
CREATE INDEX det_order ON detections(media_id, media_time_us, detection_id);
CREATE INDEX det_class ON detections(class, confidence);
CREATE INDEX det_source ON detections(source_id, class);
CREATE INDEX det_model ON detections(model_version);
CREATE INDEX det_track ON detections(track_id);
CREATE INDEX det_obs ON detections(observation_id);
CREATE INDEX det_utc ON detections(capture_time_utc_us);
CREATE TABLE relations(relation_id TEXT PRIMARY KEY,
  observation_id TEXT NOT NULL REFERENCES observations(observation_id),
  source_id TEXT NOT NULL, media_id TEXT NOT NULL, media_time_us INTEGER NOT NULL,
  capture_time_utc_us INTEGER, predicate TEXT NOT NULL, object_class TEXT NOT NULL,
  subject_detection_id TEXT NOT NULL, subject_track_id TEXT, confidence REAL NOT NULL,
  module_version TEXT NOT NULL, body TEXT NOT NULL);
CREATE INDEX rel_order ON relations(media_id, media_time_us, relation_id);
CREATE INDEX rel_predicate ON relations(predicate, object_class);
CREATE INDEX rel_obs ON relations(observation_id);
CREATE INDEX rel_track ON relations(subject_track_id);
CREATE TABLE frame_summaries(observation_id TEXT PRIMARY KEY REFERENCES observations(observation_id),
  source_id TEXT NOT NULL, media_id TEXT NOT NULL, media_time_us INTEGER NOT NULL,
  capture_time_utc_us INTEGER, body TEXT NOT NULL);
CREATE INDEX sum_order ON frame_summaries(media_id, media_time_us, observation_id);
CREATE INDEX sum_source ON frame_summaries(source_id);
CREATE TABLE query_runs(id TEXT PRIMARY KEY, snapshot INTEGER NOT NULL, body TEXT NOT NULL);
CREATE TABLE review_matches(seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL,
  query_run_id TEXT NOT NULL, body TEXT NOT NULL, UNIQUE(query_run_id, id));
CREATE TABLE reviews(id TEXT PRIMARY KEY, version INTEGER NOT NULL, updated_utc_us INTEGER NOT NULL, body TEXT NOT NULL);
CREATE TABLE alerts(seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, rule_id TEXT NOT NULL,
  source_id TEXT NOT NULL, state TEXT NOT NULL, start_us INTEGER NOT NULL, body TEXT NOT NULL);
CREATE INDEX alert_rule ON alerts(rule_id);
CREATE INDEX alert_source ON alerts(source_id);
CREATE TABLE audit(seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, utc_us INTEGER NOT NULL,
  actor TEXT NOT NULL, action TEXT NOT NULL, targets TEXT NOT NULL, result TEXT NOT NULL,
  detail TEXT NOT NULL, body TEXT NOT NULL);
CREATE INDEX audit_actor ON audit(actor);
CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit BEGIN SELECT RAISE(ABORT, 'audit is append-only'); END;
CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit BEGIN SELECT RAISE(ABORT, 'audit is append-only'); END;
CREATE TABLE commits(seq INTEGER PRIMARY KEY AUTOINCREMENT, observation_id TEXT NOT NULL, utc_us INTEGER NOT NULL);
"#;

/// Numbered, append-only list. Never edit an applied entry; add a new one.
const MIGRATIONS: &[&str] = &[SCHEMA_V1];

pub struct Sqlite {
    conn: Mutex<Connection>,
}

fn now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

fn db(err: rusqlite::Error) -> ApiError {
    let busy = matches!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked));
    let mut e = ApiError::new(if busy { "unavailable" } else { "internal" }, err.to_string());
    e.retryable = busy;
    e
}

fn ie<E: std::fmt::Display>(err: E) -> ApiError {
    ApiError::new("internal", err.to_string())
}

fn ser<T: Serialize>(v: &T) -> StoreResult<String> {
    serde_json::to_string(v).map_err(ie)
}

fn de<T: DeserializeOwned>(s: &str) -> StoreResult<T> {
    serde_json::from_str(s).map_err(ie)
}

/// Serde unit-variant name without quotes ("available").
fn tag<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default().trim_matches('"').to_string()
}

fn get_one<T: DeserializeOwned>(c: &Connection, sql: &str, id: &str) -> StoreResult<Option<T>> {
    let s: Option<String> = c.query_row(sql, [id], |r| r.get(0)).optional().map_err(db)?;
    s.map(|s| de(&s)).transpose()
}

fn get_all<T: DeserializeOwned>(c: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> StoreResult<Vec<T>> {
    let mut st = c.prepare_cached(sql).map_err(db)?;
    let mut rows = st.query(p).map_err(db)?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(db)? {
        out.push(de(&r.get::<_, String>(0).map_err(db)?)?);
    }
    Ok(out)
}

fn migrate(c: &mut Connection) -> StoreResult<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY, applied_utc_us INTEGER NOT NULL)").map_err(db)?;
    let cur: i64 = c.query_row("SELECT COALESCE(MAX(version),0) FROM schema_migrations", [], |r| r.get(0)).map_err(db)?;
    if cur as usize > MIGRATIONS.len() {
        return Err(ApiError::new("unsupported", format!("database schema v{cur} is newer than this build (v{})", MIGRATIONS.len())));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let v = i as i64 + 1;
        if v <= cur {
            continue;
        }
        let tx = c.transaction().map_err(db)?;
        tx.execute_batch(sql).map_err(db)?;
        tx.execute("INSERT INTO schema_migrations(version, applied_utc_us) VALUES(?1, ?2)", params![v, now_us()]).map_err(db)?;
        tx.commit().map_err(db)?;
    }
    Ok(())
}

impl Sqlite {
    pub fn open(path: &Path) -> StoreResult<Self> {
        if let Some(p) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(p).map_err(ie)?;
        }
        Self::init(Connection::open(path).map_err(db)?)
    }

    pub fn memory() -> StoreResult<Self> {
        Self::init(Connection::open_in_memory().map_err(db)?)
    }

    fn init(mut c: Connection) -> StoreResult<Self> {
        c.busy_timeout(Duration::from_secs(5)).map_err(db)?;
        let _mode: String = c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)).map_err(db)?;
        c.pragma_update(None, "synchronous", "NORMAL").map_err(db)?;
        c.pragma_update(None, "foreign_keys", true).map_err(db)?;
        migrate(&mut c)?;
        Ok(Sqlite { conn: Mutex::new(c) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn snapshot_of(c: &Connection) -> StoreResult<u64> {
    c.query_row("SELECT COALESCE(MAX(seq),0) FROM commits", [], |r| r.get::<_, i64>(0)).map(|n| n as u64).map_err(db)
}

fn enc_cursor(media: &str, t: i64, id: &str) -> String {
    let raw = format!("{media}\u{1f}{t}\u{1f}{id}");
    raw.bytes().map(|b| format!("{b:02x}")).collect()
}

fn dec_cursor(s: &str) -> StoreResult<(String, i64, String)> {
    let bad = || ApiError::new("invalid_request", "invalid cursor");
    if s.len() % 2 != 0 || !s.is_ascii() {
        return Err(bad());
    }
    let bytes: Result<Vec<u8>, _> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16)).collect();
    let raw = String::from_utf8(bytes.map_err(|_| bad())?).map_err(|_| bad())?;
    let mut it = raw.splitn(3, '\u{1f}');
    let (m, t, i) = (it.next().ok_or_else(bad)?, it.next().ok_or_else(bad)?, it.next().ok_or_else(bad)?);
    Ok((m.to_string(), t.parse().map_err(|_| bad())?, i.to_string()))
}

fn seq_cursor(page: &Page) -> StoreResult<Option<i64>> {
    page.after.as_deref().map(|s| s.parse::<i64>().map_err(|_| ApiError::new("invalid_request", "invalid cursor"))).transpose()
}

/// Column names a filter applies to. Values always go through bound parameters.
struct Cols {
    source: &'static str,
    media: &'static str,
    class: Option<&'static str>,
    conf: Option<&'static str>,
    model: Option<&'static str>,
    track: Option<&'static str>,
    mtime: &'static str,
    utc: &'static str,
}

fn in_list(col: &str, vals: &[String], w: &mut Vec<String>, p: &mut Vec<Value>) {
    if vals.is_empty() {
        return;
    }
    w.push(format!("{col} IN ({})", vec!["?"; vals.len()].join(",")));
    p.extend(vals.iter().map(|v| Value::Text(v.clone())));
}

fn build_filter(f: &DetectionFilter, c: &Cols) -> (String, Vec<Value>) {
    let (mut w, mut p) = (vec!["1=1".to_string()], Vec::new());
    in_list(c.source, &f.source_ids, &mut w, &mut p);
    in_list(c.media, &f.media_ids, &mut w, &mut p);
    if let Some(col) = c.class {
        in_list(col, &f.classes, &mut w, &mut p);
    }
    if let (Some(col), Some(m)) = (c.conf, f.min_confidence) {
        w.push(format!("{col} >= ?"));
        p.push(Value::Real(m as f64));
    }
    if let Some(col) = c.model {
        in_list(col, &f.model_versions, &mut w, &mut p);
    }
    if let Some(col) = c.track {
        in_list(col, &f.track_ids, &mut w, &mut p);
    }
    for (col, op, v) in [(c.mtime, ">=", f.media_from_us), (c.mtime, "<=", f.media_to_us), (c.utc, ">=", f.utc_from_us), (c.utc, "<=", f.utc_to_us)] {
        if let Some(v) = v {
            w.push(format!("{col} {op} ?"));
            p.push(Value::Integer(v));
        }
    }
    (w.join(" AND "), p)
}

/// Keyset pagination ordered by `keys`. `select` must end with the three key columns, in order.
fn keyset<T>(
    c: &Connection,
    select: &str,
    filter: (String, Vec<Value>),
    keys: [&str; 3],
    page: &Page,
    map: impl Fn(&rusqlite::Row) -> StoreResult<T>,
) -> StoreResult<(Vec<T>, Option<String>)> {
    let (mut w, mut p) = filter;
    if let Some(a) = &page.after {
        let (m, t, i) = dec_cursor(a)?;
        w.push_str(&format!(" AND ({},{},{}) > (?,?,?)", keys[0], keys[1], keys[2]));
        p.extend([Value::Text(m), Value::Integer(t), Value::Text(i)]);
    }
    let limit = page.limit.clamp(1, 10_000) as usize;
    let sql = format!("{select} WHERE {w} ORDER BY {},{},{} LIMIT {}", keys[0], keys[1], keys[2], limit + 1);
    let mut st = c.prepare_cached(&sql).map_err(db)?;
    let mut rows = st.query(params_from_iter(p.iter())).map_err(db)?;
    let mut out = Vec::new();
    let mut last = None;
    let mut more = false;
    while let Some(r) = rows.next().map_err(db)? {
        if out.len() == limit {
            more = true;
            break;
        }
        let n = r.as_ref().column_count();
        last = Some(enc_cursor(&r.get::<_, String>(n - 3).map_err(db)?, r.get(n - 2).map_err(db)?, &r.get::<_, String>(n - 1).map_err(db)?));
        out.push(map(r)?);
    }
    Ok((out, if more { last } else { None }))
}

fn write_job(c: &Connection, j: &IndexJob) -> StoreResult<()> {
    c.execute(
        "INSERT INTO jobs(id,state,created_utc_us,checkpoint,coverage,body) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(id) DO UPDATE SET state=excluded.state, checkpoint=excluded.checkpoint, coverage=excluded.coverage, body=excluded.body",
        params![j.id, tag(&j.state), j.created_utc_us, ser(&j.checkpoint)?, ser(&j.coverage)?, ser(j)?],
    )
    .map_err(db)?;
    Ok(())
}

impl Store for Sqlite {
    fn upsert_source(&self, s: &Source) -> StoreResult<()> {
        self.lock()
            .execute("INSERT INTO sources(id,body) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body", params![s.id, ser(s)?])
            .map_err(db)?;
        Ok(())
    }

    fn sources(&self) -> StoreResult<Vec<Source>> {
        get_all(&self.lock(), "SELECT body FROM sources ORDER BY id", &[])
    }

    fn upsert_media(&self, m: &MediaAsset) -> StoreResult<()> {
        self.lock()
            .execute(
                "INSERT INTO media(id,source_id,availability,body) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(id) DO UPDATE SET source_id=excluded.source_id, availability=excluded.availability, body=excluded.body",
                params![m.id, m.source_id, tag(&m.availability), ser(m)?],
            )
            .map_err(db)?;
        Ok(())
    }

    fn media(&self, id: &str) -> StoreResult<Option<MediaAsset>> {
        get_one(&self.lock(), "SELECT body FROM media WHERE id=?1", id)
    }

    fn media_for_source(&self, source_id: &str) -> StoreResult<Vec<MediaAsset>> {
        get_all(&self.lock(), "SELECT body FROM media WHERE source_id=?1 ORDER BY id", &[&source_id])
    }

    fn upsert_model(&self, m: &ModelVersion) -> StoreResult<()> {
        self.lock()
            .execute("INSERT INTO model_versions(id,body) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body", params![m.id, ser(m)?])
            .map_err(db)?;
        Ok(())
    }

    fn models(&self) -> StoreResult<Vec<ModelVersion>> {
        get_all(&self.lock(), "SELECT body FROM model_versions ORDER BY id", &[])
    }

    fn upsert_job(&self, j: &IndexJob) -> StoreResult<()> {
        write_job(&self.lock(), j)
    }

    fn job(&self, id: &str) -> StoreResult<Option<IndexJob>> {
        get_one(&self.lock(), "SELECT body FROM jobs WHERE id=?1", id)
    }

    fn jobs(&self) -> StoreResult<Vec<IndexJob>> {
        get_all(&self.lock(), "SELECT body FROM jobs ORDER BY created_utc_us DESC, id", &[])
    }

    fn commit_batch(&self, b: &IndexBatch) -> StoreResult<BatchOutcome> {
        let o = &b.observation;
        if b.schema_version != SCHEMA_VERSION {
            return Err(ApiError::new("invalid_request", format!("unsupported schema_version {}", b.schema_version)));
        }
        if b.detections.iter().any(|d| d.observation_id != o.observation_id) || b.relations.iter().any(|r| r.observation_id != o.observation_id) {
            return Err(ApiError::new("invalid_request", "detection/relation observation_id does not match the batch observation"));
        }
        let mut c = self.lock();
        let tx = c.transaction().map_err(db)?;
        let mut out = BatchOutcome::default();
        out.inserted_observations = tx
            .execute(
                "INSERT OR IGNORE INTO observations(observation_id,source_id,media_id,job_id,media_time_us,capture_time_utc_us,indexed_at_utc_us,body)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![o.observation_id, o.source_id, o.media_id, o.job_id, o.media_time_us, o.capture_time_utc_us, o.indexed_at_utc_us, ser(o)?],
            )
            .map_err(db)? as u32;
        for d in &b.detections {
            out.inserted_detections += tx
                .execute(
                    "INSERT OR IGNORE INTO detections(detection_id,observation_id,source_id,media_id,media_time_us,capture_time_utc_us,class,confidence,model_version,track_id,body)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![d.detection_id, o.observation_id, o.source_id, o.media_id, o.media_time_us, o.capture_time_utc_us, d.class, d.confidence as f64, d.model_version, d.track_id, ser(d)?],
                )
                .map_err(db)? as u32;
        }
        for r in &b.relations {
            out.inserted_relations += tx
                .execute(
                    "INSERT OR IGNORE INTO relations(relation_id,observation_id,source_id,media_id,media_time_us,capture_time_utc_us,predicate,object_class,subject_detection_id,subject_track_id,confidence,module_version,body)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    params![r.relation_id, o.observation_id, o.source_id, o.media_id, o.media_time_us, o.capture_time_utc_us, tag(&r.predicate), r.object_class, r.subject_detection_id, r.subject_track_id, r.confidence as f64, r.module_version, ser(r)?],
                )
                .map_err(db)? as u32;
        }
        if out.inserted_observations + out.inserted_detections + out.inserted_relations == 0 {
            out.duplicate = true;
            out.snapshot = snapshot_of(&tx)?;
            return Ok(out); // nothing written; dropping tx rolls back
        }

        // Summary from everything now stored for this observation (a late partial retry still converges).
        let mut classes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut min_conf: Option<f32> = None;
        {
            let mut st = tx.prepare_cached("SELECT class, detection_id, confidence FROM detections WHERE observation_id=?1").map_err(db)?;
            let mut rows = st.query([&o.observation_id]).map_err(db)?;
            while let Some(r) = rows.next().map_err(db)? {
                classes.entry(r.get(0).map_err(db)?).or_default().insert(r.get(1).map_err(db)?);
                let cf = r.get::<_, f64>(2).map_err(db)? as f32;
                min_conf = Some(min_conf.map_or(cf, |m| m.min(cf)));
            }
        }
        let mut rels: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for r in get_all::<Relation>(&tx, "SELECT body FROM relations WHERE observation_id=?1", &[&o.observation_id])? {
            rels.entry(relation_key(r.predicate, &r.object_class)).or_default().insert(r.subject_detection_id);
        }
        let to_counts = |m: BTreeMap<String, BTreeSet<String>>| m.into_iter().map(|(k, v)| (k, Count::Known(v.len() as u32))).collect::<BTreeMap<_, _>>();
        let summary = FrameSummary {
            observation_id: o.observation_id.clone(),
            source_id: o.source_id.clone(),
            media_id: o.media_id.clone(),
            media_time_us: o.media_time_us,
            capture_time_utc_us: o.capture_time_utc_us,
            zone: None,
            min_confidence: min_conf.unwrap_or(0.0),
            class_counts: to_counts(classes),
            relation_counts: to_counts(rels),
            quality: o.quality,
        };
        tx.execute(
            "INSERT INTO frame_summaries(observation_id,source_id,media_id,media_time_us,capture_time_utc_us,body) VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(observation_id) DO UPDATE SET body=excluded.body",
            params![o.observation_id, o.source_id, o.media_id, o.media_time_us, o.capture_time_utc_us, ser(&summary)?],
        )
        .map_err(db)?;
        tx.execute("INSERT INTO commits(observation_id, utc_us) VALUES(?1,?2)", params![o.observation_id, now_us()]).map_err(db)?;
        out.snapshot = tx.last_insert_rowid() as u64;

        // Checkpoint advances only here, after the rows above are in the same transaction.
        if let Some(mut job) = get_one::<IndexJob>(&tx, "SELECT body FROM jobs WHERE id=?1", &o.job_id)? {
            let cur = job.checkpoint.get(&o.media_id).copied().unwrap_or(i64::MIN);
            if o.media_time_us > cur {
                job.checkpoint.insert(o.media_id.clone(), o.media_time_us);
                write_job(&tx, &job)?;
            }
        }
        tx.commit().map_err(db)?;
        Ok(out)
    }

    fn detections(&self, f: &DetectionFilter, page: &Page) -> StoreResult<(Vec<(FrameObservation, Detection)>, Option<String>)> {
        let cols = Cols { source: "d.source_id", media: "d.media_id", class: Some("d.class"), conf: Some("d.confidence"), model: Some("d.model_version"), track: Some("d.track_id"), mtime: "d.media_time_us", utc: "d.capture_time_utc_us" };
        keyset(
            &self.lock(),
            "SELECT o.body, d.body, d.media_id, d.media_time_us, d.detection_id FROM detections d JOIN observations o ON o.observation_id=d.observation_id",
            build_filter(f, &cols),
            ["d.media_id", "d.media_time_us", "d.detection_id"],
            page,
            |r| Ok((de(&r.get::<_, String>(0).map_err(db)?)?, de(&r.get::<_, String>(1).map_err(db)?)?)),
        )
    }

    fn count_detections(&self, f: &DetectionFilter) -> StoreResult<u64> {
        let cols = Cols { source: "source_id", media: "media_id", class: Some("class"), conf: Some("confidence"), model: Some("model_version"), track: Some("track_id"), mtime: "media_time_us", utc: "capture_time_utc_us" };
        let (w, p) = build_filter(f, &cols);
        let n: i64 = self.lock().query_row(&format!("SELECT COUNT(*) FROM detections WHERE {w}"), params_from_iter(p.iter()), |r| r.get(0)).map_err(db)?;
        Ok(n as u64)
    }

    fn observations(&self, media_id: &str, from_us: Us, to_us: Us) -> StoreResult<Vec<(FrameObservation, Vec<Detection>, Vec<Relation>)>> {
        let c = self.lock();
        let obs: Vec<FrameObservation> = get_all(&c, "SELECT body FROM observations WHERE media_id=?1 AND media_time_us BETWEEN ?2 AND ?3 ORDER BY media_time_us, observation_id", &[&media_id, &from_us, &to_us])?;
        let mut out = Vec::with_capacity(obs.len());
        for o in obs {
            let d = get_all(&c, "SELECT body FROM detections WHERE observation_id=?1 ORDER BY rowid", &[&o.observation_id])?;
            let r = get_all(&c, "SELECT body FROM relations WHERE observation_id=?1 ORDER BY rowid", &[&o.observation_id])?;
            out.push((o, d, r));
        }
        Ok(out)
    }

    /// `classes` keeps frames whose summary contains any listed class; confidence/model/track filters do not apply.
    fn summaries(&self, f: &DetectionFilter, page: &Page) -> StoreResult<(Vec<FrameSummary>, Option<String>)> {
        let cols = Cols { source: "source_id", media: "media_id", class: None, conf: None, model: None, track: None, mtime: "media_time_us", utc: "capture_time_utc_us" };
        let (mut w, mut p) = build_filter(f, &cols);
        if !f.classes.is_empty() {
            w.push_str(&format!(" AND EXISTS(SELECT 1 FROM json_each(json_extract(body,'$.class_counts')) WHERE key IN ({}))", vec!["?"; f.classes.len()].join(",")));
            p.extend(f.classes.iter().map(|c| Value::Text(c.clone())));
        }
        keyset(&self.lock(), "SELECT body, media_id, media_time_us, observation_id FROM frame_summaries", (w, p), ["media_id", "media_time_us", "observation_id"], page, |r| de(&r.get::<_, String>(0).map_err(db)?))
    }

    /// `classes` filters the object class; `model_versions` the relation module; `track_ids` the subject track.
    fn relations(&self, f: &DetectionFilter, predicate: Option<Predicate>, page: &Page) -> StoreResult<(Vec<Relation>, Option<String>)> {
        let cols = Cols { source: "source_id", media: "media_id", class: Some("object_class"), conf: Some("confidence"), model: Some("module_version"), track: Some("subject_track_id"), mtime: "media_time_us", utc: "capture_time_utc_us" };
        let (mut w, mut p) = build_filter(f, &cols);
        if let Some(pr) = predicate {
            w.push_str(" AND predicate = ?");
            p.push(Value::Text(tag(&pr)));
        }
        keyset(&self.lock(), "SELECT body, media_id, media_time_us, relation_id FROM relations", (w, p), ["media_id", "media_time_us", "relation_id"], page, |r| de(&r.get::<_, String>(0).map_err(db)?))
    }

    fn save_query_run(&self, q: &QueryRun, matches: &[ReviewMatch]) -> StoreResult<()> {
        let mut c = self.lock();
        let tx = c.transaction().map_err(db)?;
        tx.execute(
            "INSERT INTO query_runs(id,snapshot,body) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET snapshot=excluded.snapshot, body=excluded.body",
            params![q.id, q.snapshot as i64, ser(q)?],
        )
        .map_err(db)?;
        tx.execute("DELETE FROM review_matches WHERE query_run_id=?1", [&q.id]).map_err(db)?;
        for m in matches {
            tx.execute("INSERT INTO review_matches(id,query_run_id,body) VALUES(?1,?2,?3)", params![m.id, q.id, ser(m)?]).map_err(db)?;
        }
        tx.commit().map_err(db)
    }

    fn query_run(&self, id: &str) -> StoreResult<Option<QueryRun>> {
        get_one(&self.lock(), "SELECT body FROM query_runs WHERE id=?1", id)
    }

    fn matches(&self, query_run_id: &str, page: &Page) -> StoreResult<(Vec<ReviewMatch>, Option<String>)> {
        let after = seq_cursor(page)?.unwrap_or(0);
        let limit = page.limit.clamp(1, 10_000) as i64;
        let c = self.lock();
        let mut st = c.prepare_cached("SELECT seq, body FROM review_matches WHERE query_run_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3").map_err(db)?;
        let mut rows = st.query(params![query_run_id, after, limit + 1]).map_err(db)?;
        let (mut out, mut last, mut more) = (Vec::new(), None, false);
        while let Some(r) = rows.next().map_err(db)? {
            if out.len() as i64 == limit {
                more = true;
                break;
            }
            last = Some(r.get::<_, i64>(0).map_err(db)?.to_string());
            out.push(de(&r.get::<_, String>(1).map_err(db)?)?);
        }
        Ok((out, if more { last } else { None }))
    }

    /// Match IDs are unique per query run; if several runs reuse one ID the newest wins.
    fn review_match(&self, id: &str) -> StoreResult<Option<ReviewMatch>> {
        get_one(&self.lock(), "SELECT body FROM review_matches WHERE id=?1 ORDER BY seq DESC LIMIT 1", id)
    }

    fn save_review(&self, r: &ReviewSelection, expected_version: Option<u64>) -> StoreResult<ReviewSelection> {
        let mut c = self.lock();
        let tx = c.transaction().map_err(db)?;
        let cur: Option<i64> = tx.query_row("SELECT version FROM reviews WHERE id=?1", [&r.id], |x| x.get(0)).optional().map_err(db)?;
        let conflict = |m: String| {
            let mut e = ApiError::new("conflict", m);
            e.retryable = false;
            e
        };
        let next = match (cur, expected_version) {
            (None, None) => 1,
            (Some(v), Some(e)) if v as u64 == e => v as u64 + 1,
            (Some(v), Some(e)) => return Err(conflict(format!("review {} is at version {v}, request expected {e}", r.id))),
            (Some(v), None) => return Err(conflict(format!("review {} already exists at version {v}; pass expected_version", r.id))),
            (None, Some(e)) => return Err(conflict(format!("review {} does not exist, request expected version {e}", r.id))),
        };
        let mut saved = r.clone();
        saved.version = next;
        tx.execute(
            "INSERT INTO reviews(id,version,updated_utc_us,body) VALUES(?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET version=excluded.version, updated_utc_us=excluded.updated_utc_us, body=excluded.body",
            params![saved.id, next as i64, saved.updated_utc_us, ser(&saved)?],
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(saved)
    }

    fn review(&self, id: &str) -> StoreResult<Option<ReviewSelection>> {
        get_one(&self.lock(), "SELECT body FROM reviews WHERE id=?1", id)
    }

    fn reviews(&self) -> StoreResult<Vec<ReviewSelection>> {
        get_all(&self.lock(), "SELECT body FROM reviews ORDER BY updated_utc_us DESC, id", &[])
    }

    fn upsert_alert(&self, a: &Alert) -> StoreResult<()> {
        self.lock()
            .execute(
                "INSERT INTO alerts(id,rule_id,source_id,state,start_us,body) VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(id) DO UPDATE SET state=excluded.state, body=excluded.body",
                params![a.id, a.rule_id, a.source_id, tag(&a.state), a.start_us, ser(a)?],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Newest first; cursor is the last returned sequence.
    fn alerts(&self, page: &Page) -> StoreResult<(Vec<Alert>, Option<String>)> {
        seq_desc(&self.lock(), "SELECT seq, body FROM alerts WHERE seq<?1 ORDER BY seq DESC LIMIT ?2", page, &[])
    }

    /// Append-only (triggers reject UPDATE/DELETE). A repeated `id` is ignored, which makes legacy replays safe.
    fn audit(&self, r: &AuditRecord) -> StoreResult<()> {
        self.lock()
            .execute(
                "INSERT OR IGNORE INTO audit(id,utc_us,actor,action,targets,result,detail,body) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![r.id, r.utc_us, r.actor, r.action, r.targets.join(" "), r.result, r.detail, ser(r)?],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Newest first. `text` is a case-insensitive substring over actor/action/targets/result/detail.
    fn audit_log(&self, text: Option<&str>, page: &Page) -> StoreResult<(Vec<AuditRecord>, Option<String>)> {
        match text.filter(|t| !t.is_empty()) {
            None => seq_desc(&self.lock(), "SELECT seq, body FROM audit WHERE seq<?1 ORDER BY seq DESC LIMIT ?2", page, &[]),
            Some(t) => seq_desc(
                &self.lock(),
                "SELECT seq, body FROM audit WHERE seq<?1 AND instr(lower(actor||' '||action||' '||targets||' '||result||' '||detail), lower(?3))>0 ORDER BY seq DESC LIMIT ?2",
                page,
                &[&t],
            ),
        }
    }

    fn snapshot(&self) -> StoreResult<u64> {
        snapshot_of(&self.lock())
    }
}

/// `sql` binds ?1 = cursor, ?2 = limit+1, ?3.. = `extra`.
fn seq_desc<T: DeserializeOwned>(c: &Connection, sql: &str, page: &Page, extra: &[&dyn rusqlite::ToSql]) -> StoreResult<(Vec<T>, Option<String>)> {
    let before = seq_cursor(page)?.unwrap_or(i64::MAX);
    let limit = page.limit.clamp(1, 10_000) as i64;
    let mut all: Vec<&dyn rusqlite::ToSql> = vec![&before];
    let lim = limit + 1;
    all.push(&lim);
    all.extend_from_slice(extra);
    let mut st = c.prepare_cached(sql).map_err(db)?;
    let mut rows = st.query(all.as_slice()).map_err(db)?;
    let (mut out, mut last, mut more) = (Vec::new(), None, false);
    while let Some(r) = rows.next().map_err(db)? {
        if out.len() as i64 == limit {
            more = true;
            break;
        }
        last = Some(r.get::<_, i64>(0).map_err(db)?.to_string());
        out.push(de(&r.get::<_, String>(1).map_err(db)?)?);
    }
    Ok((out, if more { last } else { None }))
}
