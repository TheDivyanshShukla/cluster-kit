//! Restartable, idempotent import of the legacy JSONL journals (seen/events/audit).
//!
//! Legacy files are only read, never modified. Progress (byte offset per file) lives in the
//! `legacy-import` job checkpoint (key `offset:<file>`), so a rerun resumes after the last
//! complete line. Re-processed lines are harmless: IDs derive from content.
//!
//! Legacy `ts` is processing wall-clock, so it becomes `indexed_at_utc_us`; `capture_time_utc_us`
//! stays None. `at` is media seconds. Each camera becomes a legacy media "legacy:<camera_id>".

use crate::watch::domain::*;
use crate::watch::media_catalog::media_id;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const JOB_ID: &str = "legacy-import";
const MODEL: &str = "legacy";

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct ImportReport {
    /// seen.jsonl detections newly stored.
    pub imported: u64,
    /// seen.jsonl rows already present.
    pub duplicates: u64,
    /// Unparseable, invalid, or torn (unterminated) lines, skipped.
    pub malformed: u64,
    /// events.jsonl / audit.jsonl lines processed (upserted idempotently).
    pub events: u64,
    pub audit: u64,
    pub bytes_read: u64,
}

fn us(secs: f64) -> i64 {
    (secs * 1e6).round() as i64
}

fn save_offset(store: &dyn Store, key: &str, off: u64, media: &BTreeSet<String>, state: JobState) -> StoreResult<()> {
    let mut j = store.job(JOB_ID)?.ok_or_else(|| ApiError::new("internal", "legacy job vanished"))?;
    j.checkpoint.insert(key.to_string(), off as i64);
    j.media_ids = media.iter().cloned().collect();
    j.state = state;
    store.upsert_job(&j)
}

pub fn import(store: &dyn Store, data_dir: &Path) -> StoreResult<ImportReport> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0);
    if store.job(JOB_ID)?.is_none() {
        store.upsert_job(&IndexJob {
            id: JOB_ID.into(),
            manifest_version: "legacy-import@1".into(),
            media_ids: vec![],
            model_versions: vec![MODEL.into()],
            sampling: serde_json::json!({}),
            state: JobState::Running,
            checkpoint: BTreeMap::new(),
            coverage: Coverage::default(),
            error: None,
            created_utc_us: now,
            finished_utc_us: None,
        })?;
    }
    let mut rep = ImportReport::default();
    let mut known_sources: HashSet<String> = store.sources()?.into_iter().map(|s| s.id).collect();
    let mut media: BTreeSet<String> = store.job(JOB_ID)?.map(|j| j.media_ids.into_iter().collect()).unwrap_or_default();
    let mut seen_cams: HashSet<String> = HashSet::new();

    for name in ["seen.jsonl", "events.jsonl", "audit.jsonl"] {
        let path = data_dir.join(name);
        if !path.is_file() {
            continue;
        }
        let key = format!("offset:{name}");
        let off = store.job(JOB_ID)?.and_then(|j| j.checkpoint.get(&key).copied()).unwrap_or(0).max(0) as u64;
        let mut f = std::fs::File::open(&path).map_err(|e| ApiError::new("internal", format!("{name}: {e}")))?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        // A file that shrank was rotated/replaced: start over (content-derived IDs keep this safe).
        let off = if off > len { 0 } else { off };
        f.seek(SeekFrom::Start(off)).map_err(|e| ApiError::new("internal", e.to_string()))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| ApiError::new("internal", e.to_string()))?;
        rep.bytes_read += buf.len() as u64;

        let (mut pos, mut lines) = (0usize, 0u32);
        while let Some(nl) = buf[pos..].iter().position(|b| *b == b'\n') {
            let line = &buf[pos..pos + nl];
            let line_off = off + pos as u64;
            pos += nl + 1;
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let v: Option<serde_json::Value> = serde_json::from_slice(line).ok().filter(|v: &serde_json::Value| v.is_object());
            let ok = match (name, v) {
                (_, None) => false,
                ("seen.jsonl", Some(v)) => seen_row(store, &v, &mut rep, &mut known_sources, &mut seen_cams, &mut media)?,
                ("events.jsonl", Some(v)) => event_row(store, &v, &mut rep)?,
                (_, Some(v)) => audit_row(store, &v, line_off, &mut rep)?,
            };
            if !ok {
                rep.malformed += 1;
            }
            lines += 1;
            if lines % 1000 == 0 {
                save_offset(store, &key, off + pos as u64, &media, JobState::Running)?;
            }
        }
        // Unterminated tail = torn write. Count it, but leave the offset before it so a completed line is picked up later.
        if buf[pos..].iter().any(|b| !b.is_ascii_whitespace()) {
            rep.malformed += 1;
        }
        save_offset(store, &key, off + pos as u64, &media, JobState::Running)?;
    }
    let mut j = store.job(JOB_ID)?.ok_or_else(|| ApiError::new("internal", "legacy job vanished"))?;
    j.state = JobState::Completed;
    j.finished_utc_us = Some(now);
    store.upsert_job(&j)?;
    Ok(rep)
}

fn seen_row(
    store: &dyn Store,
    v: &serde_json::Value,
    rep: &mut ImportReport,
    known_sources: &mut HashSet<String>,
    seen_cams: &mut HashSet<String>,
    media: &mut BTreeSet<String>,
) -> StoreResult<bool> {
    let cam = v["camera_id"].as_str().filter(|s| !s.is_empty());
    let at = v["at"].as_f64().filter(|n| n.is_finite() && *n >= 0.0);
    let ts = v["ts"].as_f64().filter(|n| n.is_finite() && *n > 0.0);
    let class = v["class"].as_str().filter(|s| !s.is_empty());
    let id = v["id"].as_u64();
    let (Some(cam), Some(at), Some(ts), Some(class), Some(id)) = (cam, at, ts, class, id) else { return Ok(false) };
    let seq = v["seq"].as_u64().unwrap_or(0);
    let fp = format!("legacy:{cam}");
    let mid = media_id(cam, &fp);
    if seen_cams.insert(cam.to_string()) {
        if known_sources.insert(cam.to_string()) {
            store.upsert_source(&Source {
                id: cam.into(),
                name: v["camera"].as_str().unwrap_or(cam).into(),
                kind: SourceKind::Archive,
                location: format!("legacy:{cam}"),
                credential_ref: None,
                eligible_workers: vec![],
                enabled: false,
                capture_timezone: None,
            })?;
        }
        store.upsert_media(&MediaAsset {
            id: mid.clone(), source_id: cam.into(), uri: format!("legacy:{cam}"), fingerprint: fp.clone(),
            duration_us: None, width: None, height: None, codec: None, timebase: None, capture_start_utc_us: None,
            proxy: ProxyStatus::None, availability: Availability::Unknown, still: false,
        })?;
    }
    media.insert(mid.clone());
    let key = if seq > 0 { format!("s{seq}") } else { format!("a{at:.6}") };
    let mt = us(at);
    let oid = observation_id(&fp, mt, &format!("legacy@{ts:.6}#{key}"));
    let model = v["model"].as_str().filter(|s| !s.is_empty()).unwrap_or(MODEL).to_string();
    let track = v["track_id"].as_u64().filter(|t| *t > 0).map(|t| format!("legacy:{cam}:{t}"));
    let batch = IndexBatch {
        schema_version: SCHEMA_VERSION,
        lease: None,
        observation: FrameObservation {
            observation_id: oid.clone(), source_id: cam.into(), media_id: mid, job_id: JOB_ID.into(),
            frame_index: None, pts: None, timebase: None, media_time_us: mt, capture_time_utc_us: None,
            indexed_at_utc_us: us(ts), model_versions: vec![model.clone()], quality: Quality::Ok,
            thumbnail: v["thumbnail"].as_str().filter(|s| !s.is_empty()).map(String::from),
        },
        detections: vec![Detection {
            detection_id: detection_id(&oid, &model, id as usize),
            observation_id: oid,
            class: class.into(),
            confidence: v["confidence"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32,
            bbox: [0.0; 4],
            model_version: model,
            track_id: track,
            provenance: "legacy".into(),
            color: v["color"].as_str().filter(|s| !s.is_empty()).map(String::from),
        }],
        relations: vec![],
        thumbnail_hex: None,
    };
    let out = store.commit_batch(&batch)?;
    if out.inserted_detections > 0 { rep.imported += 1 } else { rep.duplicates += 1 }
    Ok(true)
}

fn event_row(store: &dyn Store, v: &serde_json::Value, rep: &mut ImportReport) -> StoreResult<bool> {
    let (Some(id), Some(ts)) = (v["id"].as_u64(), v["ts"].as_f64().filter(|t| t.is_finite())) else { return Ok(false) };
    let cam = v["camera"].as_str().unwrap_or("");
    let status = v["status"].as_str().unwrap_or("");
    let resolved = matches!(status, "resolved" | "closed" | "cleared" | "ended");
    store.upsert_alert(&Alert {
        id: format!("legacy_event_{id}"),
        rule_id: v["rule"].as_str().unwrap_or("legacy").into(),
        rule_version: "legacy".into(),
        source_id: cam.into(),
        media_id: None,
        start_us: 0,
        end_us: None,
        clock: ClockDomain::Media { media_id: media_id(cam, &format!("legacy:{cam}")) },
        values: serde_json::json!({"key": v["key"], "status": status, "note": v["note"], "processing_ts": ts}),
        severity: "info".into(),
        evidence: vec![],
        state: if resolved { AlertState::Resolved } else { AlertState::Active },
        decision: Decision::Unreviewed,
        actor: None,
        updated_utc_us: us(ts),
    })?;
    rep.events += 1;
    Ok(true)
}

fn audit_row(store: &dyn Store, v: &serde_json::Value, line_off: u64, rep: &mut ImportReport) -> StoreResult<bool> {
    let (Some(ts), Some(action)) = (v["ts"].as_f64().filter(|t| t.is_finite()), v["action"].as_str()) else { return Ok(false) };
    store.audit(&AuditRecord {
        id: format!("legacy_audit_{line_off}"),
        utc_us: us(ts),
        actor: "legacy".into(),
        action: action.into(),
        targets: vec![],
        result: "ok".into(),
        correlation_id: None,
        detail: v["detail"].as_str().unwrap_or("").into(),
    })?;
    rep.audit += 1;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::storage::tests::tmp;

    fn repo_seen() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../vision/data/seen.jsonl")
    }

    #[test]
    fn legacy_import_counts_and_resumes() {
        let dir = tmp("legacy");
        let text = std::fs::read_to_string(repo_seen()).unwrap();
        let valid = text.lines().filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()).count() as u64;
        std::fs::write(dir.join("seen.jsonl"), &text).unwrap();
        let store = crate::watch::storage::memory();
        let r = import(&*store, &dir).unwrap();
        assert_eq!(r.imported, valid);
        assert_eq!(r.malformed, 0);
        assert_eq!(store.count_detections(&DetectionFilter::default()).unwrap(), valid);
        // rerun: nothing new, nothing duplicated
        let r2 = import(&*store, &dir).unwrap();
        assert_eq!((r2.imported, r2.duplicates, r2.malformed), (0, 0, 0));
        assert_eq!(store.count_detections(&DetectionFilter::default()).unwrap(), valid);
        // capture time is never fabricated from legacy ts
        let (rows, _) = store.detections(&DetectionFilter::default(), &Page { after: None, limit: 5 }).unwrap();
        assert!(rows.iter().all(|(o, _)| o.capture_time_utc_us.is_none() && o.indexed_at_utc_us > 0));
        // original file untouched
        assert_eq!(std::fs::read_to_string(dir.join("seen.jsonl")).unwrap(), text);
    }

    #[test]
    fn legacy_torn_and_malformed_lines_skipped_then_completed() {
        let dir = tmp("legacy_torn");
        let good = r#"{"at":1.5,"camera":"Hall","camera_id":"hall","class":"person","color":"red","id":1,"ts":1792963258.5}"#;
        let good2 = r#"{"at":2.5,"camera":"Hall","camera_id":"hall","class":"car","color":"red","id":2,"ts":1792963259.5}"#;
        let torn = &good2[..30];
        std::fs::write(dir.join("seen.jsonl"), format!("{good}\nnot json\n{{\"x\":1}}\n{torn}")).unwrap();
        let store = crate::watch::storage::memory();
        let r = import(&*store, &dir).unwrap();
        assert_eq!((r.imported, r.malformed), (1, 3)); // garbage, missing fields, torn tail
        // the writer finishes the line: it is picked up, earlier lines are not recounted
        std::fs::write(dir.join("seen.jsonl"), format!("{good}\nnot json\n{{\"x\":1}}\n{good2}\n")).unwrap();
        let r2 = import(&*store, &dir).unwrap();
        assert_eq!((r2.imported, r2.malformed), (1, 0));
        assert_eq!(store.count_detections(&DetectionFilter::default()).unwrap(), 2);
    }

    #[test]
    fn legacy_events_and_audit() {
        let dir = tmp("legacy_ea");
        std::fs::write(dir.join("events.jsonl"), "{\"id\":7,\"ts\":1792963258.5,\"key\":\"k\",\"rule\":\"r\",\"camera\":\"Hall\",\"status\":\"open\",\"note\":\"n\"}\n").unwrap();
        std::fs::write(dir.join("audit.jsonl"), "{\"ts\":1792963258.5,\"action\":\"start\",\"detail\":\"d\"}\n").unwrap();
        let store = crate::watch::storage::memory();
        let r = import(&*store, &dir).unwrap();
        assert_eq!((r.events, r.audit), (1, 1));
        import(&*store, &dir).unwrap();
        let pg = Page { after: None, limit: 10 };
        assert_eq!(store.alerts(&pg).unwrap().0.len(), 1);
        assert_eq!(store.audit_log(Some("START"), &pg).unwrap().0.len(), 1);
        assert_eq!(store.audit_log(None, &pg).unwrap().0.len(), 1);
    }
}
