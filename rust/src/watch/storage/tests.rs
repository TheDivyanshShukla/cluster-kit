use super::*;
use crate::watch::domain::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

static N: AtomicU32 = AtomicU32::new(0);

/// Unique temp dir under std::env::temp_dir.
pub fn tmp(name: &str) -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("cw-a-{name}-{}-{nanos}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

const CLS: [&str; 3] = ["person", "car", "dog"];

fn conf(i: usize, j: usize) -> f32 {
    0.30 + ((i * 7 + j * 13) % 70) as f32 / 100.0
}

fn batch(i: usize, media: &str, job: &str) -> IndexBatch {
    let t = i as i64 * 1000;
    let oid = observation_id(media, t, "test@1");
    let dets: Vec<Detection> = (0..3)
        .map(|j| Detection {
            detection_id: detection_id(&oid, "m1", j), observation_id: oid.clone(), class: CLS[(i + j) % 3].into(),
            confidence: conf(i, j), bbox: [0.1, 0.1, 0.5, 0.5], model_version: "m1".into(),
            track_id: Some(format!("t{}", j)), provenance: "model".into(), color: None,
        })
        .collect();
    let rels = vec![Relation {
        relation_id: stable_id("rel", &[&oid]), observation_id: oid.clone(), subject_detection_id: dets[0].detection_id.clone(),
        subject_track_id: None, predicate: Predicate::Near, object_detection_id: dets[1].detection_id.clone(), object_track_id: None,
        object_class: dets[1].class.clone(), confidence: 0.5, module_version: "rel@1".into(), method: "bbox-proximity".into(),
    }];
    IndexBatch {
        schema_version: SCHEMA_VERSION, lease: None,
        observation: FrameObservation {
            observation_id: oid, source_id: "src".into(), media_id: media.into(), job_id: job.into(), frame_index: None, pts: None,
            timebase: None, media_time_us: t, capture_time_utc_us: None, indexed_at_utc_us: 1, model_versions: vec!["m1".into()],
            quality: Quality::Ok, thumbnail: None,
        },
        detections: dets, relations: rels, thumbnail_hex: None,
    }
}

fn job(id: &str) -> IndexJob {
    IndexJob {
        id: id.into(), manifest_version: "v1".into(), media_ids: vec![], model_versions: vec![], sampling: serde_json::json!({}),
        state: JobState::Running, checkpoint: BTreeMap::new(), coverage: Coverage::default(), error: None, created_utc_us: 1, finished_utc_us: None,
    }
}

fn all() -> Page {
    Page { after: None, limit: 10_000 }
}

#[test]
fn retry_dedup_same_batch_twice() {
    let s = memory();
    s.upsert_job(&job("j")).unwrap();
    let b = batch(1, "m", "j");
    let a = s.commit_batch(&b).unwrap();
    assert!(!a.duplicate && (a.inserted_observations, a.inserted_detections, a.inserted_relations) == (1, 3, 1));
    let snap = s.snapshot().unwrap();
    let r = s.commit_batch(&b).unwrap();
    assert!(r.duplicate && r.inserted_detections == 0 && r.snapshot == snap);
    assert_eq!(s.snapshot().unwrap(), snap);
    assert_eq!(s.count_detections(&DetectionFilter::default()).unwrap(), 3);
    assert_eq!(s.summaries(&DetectionFilter::default(), &all()).unwrap().0.len(), 1);
}

#[test]
fn summary_counts_distinct_and_absent_classes_stay_absent() {
    let s = memory();
    let mut b = batch(0, "m", "j");
    b.detections[1].class = b.detections[0].class.clone();
    b.detections[2].class = b.detections[0].class.clone();
    b.relations[0].object_class = b.detections[0].class.clone();
    let near = b.relations[0].clone();
    let mut r2 = near.clone();
    r2.relation_id = "rel_b".into();
    r2.object_detection_id = b.detections[2].detection_id.clone(); // same subject, second relation
    r2.object_class = b.detections[0].class.clone();
    b.relations = vec![near, r2];
    s.commit_batch(&b).unwrap();
    let sum = &s.summaries(&DetectionFilter::default(), &all()).unwrap().0[0];
    let cls = &b.detections[0].class;
    assert_eq!(sum.class(cls), Count::Known(3));
    assert_eq!(sum.class("knife"), Count::Unknown);
    assert!(!sum.class_counts.contains_key("knife"));
    assert_eq!(sum.relation(Predicate::Near, cls), Count::Known(1)); // distinct subjects, not relations
    assert_eq!(sum.relation(Predicate::Holding, cls), Count::Unsupported);
    // filter by class presence
    let f = DetectionFilter { classes: vec!["knife".into()], ..Default::default() };
    assert!(s.summaries(&f, &all()).unwrap().0.is_empty());
    let f = DetectionFilter { classes: vec![cls.clone()], ..Default::default() };
    assert_eq!(s.summaries(&f, &all()).unwrap().0.len(), 1);
}

#[test]
fn checkpoint_advances_after_insert_and_never_regresses() {
    let s = memory();
    s.upsert_job(&job("j")).unwrap();
    s.commit_batch(&batch(10, "m", "j")).unwrap(); // t=10_000
    s.commit_batch(&batch(0, "m", "j")).unwrap(); // t=0
    assert_eq!(s.job("j").unwrap().unwrap().checkpoint["m"], 10_000);
    // invalid batch must not move the checkpoint
    let mut bad = batch(50, "m", "j");
    bad.detections[0].observation_id = "other".into();
    assert!(s.commit_batch(&bad).is_err());
    assert_eq!(s.job("j").unwrap().unwrap().checkpoint["m"], 10_000);
}

#[test]
fn crash_restart_keeps_data_and_snapshot() {
    let dir = tmp("restart");
    let path = dir.join("watch.db");
    let snap;
    {
        let s = open(&path).unwrap();
        s.upsert_job(&job("j")).unwrap();
        for i in 0..20 {
            s.commit_batch(&batch(i, "m", "j")).unwrap();
        }
        snap = s.snapshot().unwrap();
    } // dropped without any explicit close
    let s = open(&path).unwrap();
    assert_eq!(s.snapshot().unwrap(), snap);
    assert_eq!(s.count_detections(&DetectionFilter::default()).unwrap(), 60);
    assert_eq!(s.job("j").unwrap().unwrap().checkpoint["m"], 19_000);
    assert!(s.commit_batch(&batch(3, "m", "j")).unwrap().duplicate);
    assert_eq!(s.commit_batch(&batch(100, "m", "j")).unwrap().snapshot, snap + 1);
    let n: i64 = rusqlite::Connection::open(&path).unwrap().query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}

#[test]
fn unavailable_media_roundtrip() {
    let s = memory();
    let m = MediaAsset {
        id: "m1".into(), source_id: "s".into(), uri: "/gone/x.mp4".into(), fingerprint: "fp".into(), duration_us: Some(5), width: None,
        height: None, codec: None, timebase: None, capture_start_utc_us: None, proxy: ProxyStatus::None, availability: Availability::Available, still: false,
    };
    s.upsert_media(&m).unwrap();
    let mut gone = m.clone();
    gone.availability = Availability::Missing;
    s.upsert_media(&gone).unwrap();
    assert_eq!(s.media("m1").unwrap().unwrap().availability, Availability::Missing);
    assert_eq!(s.media_for_source("s").unwrap().len(), 1);
    assert!(s.media("nope").unwrap().is_none());
    // indexed rows survive the media going missing
    s.commit_batch(&batch(0, "m1", "j")).unwrap();
    assert_eq!(s.observations("m1", 0, 1_000_000).unwrap().len(), 1);
}

#[test]
fn review_version_conflict() {
    let s = memory();
    let r = ReviewSelection { id: "r1".into(), version: 0, title: "t".into(), items: vec![], notes: String::new(), actor: None, export: ExportStatus::None, updated_utc_us: 1 };
    assert_eq!(s.save_review(&r, Some(1)).unwrap_err().code, "conflict");
    let v1 = s.save_review(&r, None).unwrap();
    assert_eq!(v1.version, 1);
    assert_eq!(s.save_review(&r, None).unwrap_err().code, "conflict");
    let mut edit = v1.clone();
    edit.title = "new".into();
    let v2 = s.save_review(&edit, Some(1)).unwrap();
    assert_eq!(v2.version, 2);
    let e = s.save_review(&edit, Some(1)).unwrap_err(); // stale
    assert_eq!((e.code.as_str(), e.status()), ("conflict", 409));
    assert_eq!(s.review("r1").unwrap().unwrap().title, "new");
    assert_eq!(s.reviews().unwrap().len(), 1);
}

#[test]
fn matches_runs_alerts_audit_roundtrip() {
    let s = memory();
    let q: QueryRun = serde_json::from_str(include_str!("../contracts/query_run.v1.json")).unwrap();
    let m: ReviewMatch = serde_json::from_str(include_str!("../contracts/review_match.v1.json")).unwrap();
    let ms: Vec<ReviewMatch> = (0..5).map(|i| ReviewMatch { id: format!("mm{i}"), query_run_id: q.id.clone(), ..m.clone() }).collect();
    s.save_query_run(&q, &ms).unwrap();
    assert_eq!(s.query_run(&q.id).unwrap().unwrap(), q);
    let (p1, c) = s.matches(&q.id, &Page { after: None, limit: 2 }).unwrap();
    let (p2, c2) = s.matches(&q.id, &Page { after: c, limit: 2 }).unwrap();
    let (p3, c3) = s.matches(&q.id, &Page { after: c2, limit: 2 }).unwrap();
    assert_eq!((p1.len(), p2.len(), p3.len(), c3), (2, 2, 1, None));
    assert_eq!(p3[0].id, "mm4");
    // review_match lookup by id
    assert_eq!(s.review_match("mm3").unwrap().unwrap().id, "mm3");
    assert!(s.review_match("zzz").unwrap().is_none());
    // re-saving the run replaces its matches
    s.save_query_run(&q, &ms[..1]).unwrap();
    assert_eq!(s.matches(&q.id, &all()).unwrap().0.len(), 1);
    assert!(s.review_match("mm3").unwrap().is_none());

    for i in 0..5 {
        s.audit(&AuditRecord { id: format!("a{i}"), utc_us: i, actor: "u".into(), action: if i == 2 { "export".into() } else { "view".into() }, targets: vec!["x".into()], result: "ok".into(), correlation_id: None, detail: String::new() }).unwrap();
    }
    s.audit(&AuditRecord { id: "a0".into(), utc_us: 99, actor: "u".into(), action: "tamper".into(), targets: vec![], result: "ok".into(), correlation_id: None, detail: String::new() }).unwrap();
    let (log, _) = s.audit_log(None, &all()).unwrap();
    assert_eq!(log.len(), 5);
    assert_eq!(log[0].id, "a4");
    assert!(log.iter().all(|r| r.action != "tamper"));
    assert_eq!(s.audit_log(Some("EXPORT"), &all()).unwrap().0.len(), 1);
}

#[test]
fn cursor_pages_are_stable_and_complete() {
    let s = memory();
    for i in 0..60 {
        s.commit_batch(&batch(i, if i % 2 == 0 { "ma" } else { "mb" }, "j")).unwrap();
    }
    let f = DetectionFilter::default();
    let (full, none) = s.detections(&f, &all()).unwrap();
    assert_eq!((full.len(), none), (180, None));
    let key = |r: &(FrameObservation, Detection)| (r.0.media_id.clone(), r.0.media_time_us, r.1.detection_id.clone());
    assert!(full.windows(2).all(|w| key(&w[0]) < key(&w[1])));
    for limit in [1u32, 7, 50, 180, 500] {
        let (mut got, mut after) = (Vec::new(), None);
        loop {
            let (rows, next) = s.detections(&f, &Page { after: after.clone(), limit }).unwrap();
            assert!(rows.len() as u32 <= limit);
            got.extend(rows);
            if next.is_none() {
                break;
            }
            after = next;
        }
        assert_eq!(got.iter().map(key).collect::<Vec<_>>(), full.iter().map(key).collect::<Vec<_>>(), "limit {limit}");
    }
    // a commit between pages must not shift or repeat earlier rows
    let (p1, c) = s.detections(&f, &Page { after: None, limit: 10 }).unwrap();
    s.commit_batch(&batch(1000, "ma", "j")).unwrap();
    let (p2, _) = s.detections(&f, &Page { after: c, limit: 10 }).unwrap();
    assert!(key(p1.last().unwrap()) < key(&p2[0]));
    assert_eq!(s.detections(&f, &Page { after: Some("zz".into()), limit: 1 }).unwrap_err().code, "invalid_request");
    let (rels, _) = s.relations(&f, Some(Predicate::Near), &all()).unwrap();
    assert_eq!(rels.len(), 61);
    assert!(s.relations(&f, Some(Predicate::Holding), &all()).unwrap().0.is_empty());
}

#[test]
fn filter_values_are_parameters_not_sql() {
    let s = memory();
    s.commit_batch(&batch(0, "m", "j")).unwrap();
    let f = DetectionFilter { classes: vec!["x' OR '1'='1".into()], ..Default::default() };
    assert_eq!(s.count_detections(&f).unwrap(), 0);
    let f = DetectionFilter { media_ids: vec!["m'; DROP TABLE detections; --".into()], ..Default::default() };
    assert_eq!(s.count_detections(&f).unwrap(), 0);
    assert_eq!(s.count_detections(&DetectionFilter::default()).unwrap(), 3);
}

#[test]
fn counts_correct_on_generated_index_and_bench() {
    const N_OBS: usize = 5000;
    let s = memory();
    s.upsert_job(&job("j")).unwrap();
    let batches: Vec<IndexBatch> = (0..N_OBS).map(|i| batch(i, &format!("m{}", i % 5), "j")).collect();
    let t0 = Instant::now();
    for b in &batches {
        s.commit_batch(b).unwrap();
    }
    let secs = t0.elapsed().as_secs_f64();
    // retry the whole stream: nothing changes
    let snap = s.snapshot().unwrap();
    for b in &batches {
        assert!(s.commit_batch(b).unwrap().duplicate);
    }
    assert_eq!(s.snapshot().unwrap(), snap);
    assert_eq!(snap, N_OBS as u64);

    let expect = |class: &str, min: f32| (0..N_OBS).flat_map(|i| (0..3).map(move |j| (i, j))).filter(|&(i, j)| CLS[(i + j) % 3] == class && conf(i, j) >= min).count() as u64;
    for class in CLS {
        let f = DetectionFilter { classes: vec![class.into()], min_confidence: Some(0.5), ..Default::default() };
        assert_eq!(s.count_detections(&f).unwrap(), expect(class, 0.5), "{class}");
    }
    assert_eq!(s.count_detections(&DetectionFilter::default()).unwrap(), 3 * N_OBS as u64);
    // summaries agree with detections: each frame has 3 detections spread over classes
    let (sums, _) = s.summaries(&DetectionFilter::default(), &all()).unwrap();
    assert_eq!(sums.len(), N_OBS);
    let total: u32 = sums.iter().flat_map(|x| x.class_counts.values()).map(|c| c.known().unwrap()).sum();
    assert_eq!(total, 3 * N_OBS as u32);

    // filtered query latency: class + confidence + one media + time window, first 100 rows
    let f = DetectionFilter { classes: vec!["person".into()], min_confidence: Some(0.6), media_ids: vec!["m3".into()], media_from_us: Some(100_000), media_to_us: Some(4_000_000), ..Default::default() };
    let runs = 50;
    let t1 = Instant::now();
    let mut rows = 0;
    for _ in 0..runs {
        rows = s.detections(&f, &Page { after: None, limit: 100 }).unwrap().0.len();
    }
    let q_ms = t1.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    let t2 = Instant::now();
    let cnt = s.count_detections(&DetectionFilter { classes: vec!["person".into()], min_confidence: Some(0.6), ..Default::default() }).unwrap();
    let c_ms = t2.elapsed().as_secs_f64() * 1000.0;
    println!("BENCH write: {N_OBS} batches (3 det + 1 rel each) in {secs:.2}s = {:.0} batches/s (in-memory sqlite)", N_OBS as f64 / secs);
    println!("BENCH filtered page query: {q_ms:.2} ms avg over {runs} runs ({rows} rows); count person>=0.6 over {} dets: {c_ms:.2} ms (count {cnt})", 3 * N_OBS);
}

#[test]
fn bench_file_backed_write_throughput() {
    let dir = tmp("bench");
    let s = open(&dir.join("bench.db")).unwrap();
    s.upsert_job(&job("j")).unwrap();
    let n = 2000;
    let t0 = Instant::now();
    for i in 0..n {
        s.commit_batch(&batch(i, &format!("m{}", i % 5), "j")).unwrap();
    }
    let secs = t0.elapsed().as_secs_f64();
    println!("BENCH write file-backed WAL: {n} batches in {secs:.2}s = {:.0} batches/s", n as f64 / secs);
    assert_eq!(s.count_detections(&DetectionFilter::default()).unwrap(), 3 * n as u64);
}
