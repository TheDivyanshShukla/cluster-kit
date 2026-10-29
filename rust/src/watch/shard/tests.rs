//! Federated results equal single-store results; partial results, cursors and snapshots behave.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::federated::FederatedStore;
use super::node::LocalShard;
use super::*;
use crate::watch::query::v1 as q;
use crate::watch::storage::sqlite::Sqlite;

const S: i64 = 1_000_000;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cw-shard-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn asset(i: usize) -> MediaAsset {
    MediaAsset {
        id: format!("m{i}"), source_id: "cam".into(), uri: format!("/v/m{i}.mp4"), fingerprint: format!("fp{i}"), duration_us: Some(15 * S), width: None, height: None,
        codec: None, timebase: None, capture_start_utc_us: None, proxy: ProxyStatus::None, availability: Availability::Available, still: false, node_id: None,
    }
}

fn model() -> ModelVersion {
    ModelVersion {
        id: "m".into(), version: "1".into(), artifact_hash: "h".into(), adapter: AdapterKind::Object, output_schema: "yolov5".into(), labels: vec!["person".into()],
        supported_relations: vec![Predicate::Near], thresholds: BTreeMap::new(), provider: "cpu".into(), input_size: None, validation: "unvalidated".into(),
    }
}

fn job(i: usize) -> IndexJob {
    IndexJob {
        id: format!("job{i}"), manifest_version: "1".into(), media_ids: vec![format!("m{i}")], model_versions: vec!["m@1".into()], sampling: serde_json::json!({}),
        state: JobState::Completed, checkpoint: BTreeMap::new(), coverage: Coverage { decoded: 30, inferred: 30, ..Default::default() }, error: None, created_utc_us: 1, finished_utc_us: Some(2),
    }
}

/// 30 frames per recording, 0.5 s apart: a person (one track) in most, a car and a phone near the person now and then.
fn batches(i: usize) -> Vec<IndexBatch> {
    let m = asset(i);
    (0..30)
        .map(|f| {
            let t = f as i64 * S / 2;
            let oid = observation_id(&m.fingerprint, t, "a@1");
            let obs = FrameObservation {
                observation_id: oid.clone(), source_id: "cam".into(), media_id: m.id.clone(), job_id: format!("job{i}"), frame_index: Some(f as u64), pts: None, timebase: None,
                media_time_us: t, capture_time_utc_us: None, indexed_at_utc_us: 0, model_versions: vec!["m@1".into()], quality: Quality::Ok, thumbnail: None,
            };
            let det = |ord: usize, class: &str, conf: f32, track: Option<&str>| Detection {
                detection_id: detection_id(&oid, "m@1", ord), observation_id: oid.clone(), class: class.into(), confidence: conf, bbox: [0.1, 0.1, 0.4, 0.5],
                model_version: "m@1".into(), track_id: track.map(str::to_string), provenance: "model".into(), color: None,
            };
            let mut dets = vec![];
            let mut rels = vec![];
            if (f + i) % 5 != 4 {
                dets.push(det(0, "person", 0.6 + ((f * 7 + i) % 4) as f32 / 10.0, Some(&format!("t{}", (f / 10) + 1))));
            }
            if f % 3 == 0 {
                dets.push(det(1, "car", 0.7, None));
            }
            if f % 4 == 1 && !dets.is_empty() {
                dets.push(det(2, "cell phone", 0.8, None));
                let (p, ph) = (&dets[0], dets.last().unwrap());
                rels.push(Relation {
                    relation_id: stable_id("rel", &[&oid, "near"]), observation_id: oid.clone(), subject_detection_id: ph.detection_id.clone(), subject_track_id: None,
                    predicate: Predicate::Near, object_detection_id: p.detection_id.clone(), object_track_id: p.track_id.clone(), object_class: p.class.clone(),
                    confidence: 0.8, module_version: "r@1".into(), method: "bbox-proximity".into(),
                });
            }
            IndexBatch { schema_version: SCHEMA_VERSION, lease: None, observation: obs, detections: dets, relations: rels, thumbnail_hex: None }
        })
        .collect()
}

struct Rig {
    single: Arc<dyn Store>,
    fed: FederatedStore,
    shards: Vec<Arc<LocalShard>>,
    flaky: Vec<Arc<Flaky>>,
}

/// A node that can be switched off, like a killed process.
struct Flaky {
    inner: Arc<LocalShard>,
    down: AtomicBool,
}

impl Flaky {
    fn gate(&self) -> StoreResult<()> {
        if self.down.load(Ordering::SeqCst) {
            Err(unavailable("node unreachable: connection refused"))
        } else {
            Ok(())
        }
    }
}

impl NodeClient for Flaky {
    fn page(&self, k: Kind, r: &ShardReq, d: Duration) -> StoreResult<Rows> {
        self.gate()?;
        self.inner.page(k, r, d)
    }
    fn observations(&self, m: &str, a: Us, b: Us, d: Duration) -> StoreResult<Vec<ObsRow>> {
        self.gate()?;
        self.inner.observations(m, a, b, d)
    }
    fn count(&self, r: &ShardReq, d: Duration) -> StoreResult<u64> {
        self.gate()?;
        self.inner.count(r, d)
    }
    fn status(&self, d: Duration) -> StoreResult<NodeStatus> {
        self.gate()?;
        self.inner.status(d)
    }
}

fn rig(name: &str, recordings: usize) -> Rig {
    let dir = tmp(name);
    let single: Arc<dyn Store> = Arc::new(Sqlite::open(&dir.join("single.db")).unwrap());
    let cat: Arc<dyn Store> = Arc::new(Sqlite::open(&dir.join("cat.db")).unwrap());
    let shards: Vec<Arc<LocalShard>> = (0..3).map(|n| Arc::new(LocalShard::open(&format!("n{n}"), &dir.join(format!("s{n}.db"))).unwrap())).collect();
    for st in [&single, &cat] {
        st.upsert_model(&model()).unwrap();
    }
    for i in 0..recordings {
        let mut m = asset(i);
        single.upsert_media(&m).unwrap();
        m.node_id = Some(format!("n{}", i % 3));
        cat.upsert_media(&m).unwrap();
        for st in [&single, &cat] {
            st.upsert_job(&job(i)).unwrap();
        }
        shards[i % 3].store.upsert_job(&job(i)).unwrap();
        for b in batches(i) {
            single.commit_batch(&b).unwrap();
            shards[i % 3].commit(&b).unwrap();
        }
    }
    let reg = Arc::new(Registry::new());
    reg.add_local("head", Arc::new(LocalShard::over("head", cat.clone(), &dir.join("cat.db"))));
    let flaky: Vec<Arc<Flaky>> = shards.iter().map(|s| Arc::new(Flaky { inner: s.clone(), down: AtomicBool::new(false) })).collect();
    for (n, f) in flaky.iter().enumerate() {
        reg.add_local(&format!("n{n}"), f.clone());
    }
    for (n, s) in shards.iter().enumerate() {
        let st = s.status(Duration::ZERO).unwrap();
        reg.update_status(&format!("n{n}"), &st);
    }
    let fed = FederatedStore::new(cat, reg, dir);
    Rig { single, fed, shards, flaky }
}

fn req(sql: Option<&str>, lua: Option<&str>, class: Option<&str>) -> q::Request {
    q::Request {
        sql: sql.map(str::to_string), lua: lua.map(str::to_string), class: class.map(str::to_string), min_confidence: None,
        scope: QueryScope { source_ids: vec![], media_ids: vec![], from_utc_us: None, to_utc_us: None },
        gap_us: None, pre_us: None, post_us: None, limit: None, actor: None,
    }
}

/// Everything a user sees from a run, minus ids that embed the snapshot.
fn view(o: &q::Outcome) -> String {
    let mut ms: Vec<_> = o
        .matches
        .iter()
        .map(|m| format!("{} {}..{} ctx {}..{} tracks {} conf {:.3} obs {:?} det {:?} rel {:?}", m.media_id, m.start_us, m.end_us, m.context_start_us, m.context_end_us, m.distinct_tracks, m.max_confidence, m.observation_ids, m.detection_ids, m.relation_ids))
        .collect();
    ms.sort();
    format!("{} {:?} count={} tracks={:?} values={:?}\n{}", o.unit, o.run.kind, o.run.result_count, o.distinct_tracks, o.values, ms.join("\n"))
}

#[test]
fn federated_results_equal_single_store() {
    let r = rig("equal", 6);
    let reqs = [
        req(None, None, Some("person")),
        req(Some("SELECT * FROM detections WHERE class = 'car'"), None, None),
        req(Some("SELECT * FROM detections WHERE class = 'person' AND confidence >= 0.8"), None, None),
        req(Some("SELECT * FROM tracks WHERE class = 'person'"), None, None),
        req(Some("SELECT * FROM relations"), None, None),
        req(Some("SELECT * FROM frames"), None, Some("person")),
        req(Some("SELECT DISTINCT class FROM detections"), None, None),
        req(None, Some("return count(db.detections{class='person'})"), None),
        req(None, Some("for f in db.frames{} do local p = f.counts.person if p and p >= 1 and f.counts.car then emit(f) end end"), None),
    ];
    for page in [2000u32, 7] {
        let lim = q::Limits { page, ..Default::default() };
        for rq in &reqs {
            let want = q::run_with(&*r.single, rq, &lim).unwrap_or_else(|e| panic!("single {:?}: {}", rq.sql, e.message));
            let got = q::run_with(&r.fed, rq, &lim).unwrap_or_else(|e| panic!("federated {:?}: {}", rq.sql, e.message));
            assert!(want.run.result_count > 0, "non-empty: {:?} {:?}", rq.sql, rq.lua);
            assert_eq!(view(&want), view(&got), "page {page} request {:?} {:?}", rq.sql, rq.lua);
            let rep = take_run().report().expect("node report");
            assert!(!rep.partial, "all nodes answered: {:?}", rep.nodes);
            assert_eq!(rep.total, 3);
        }
    }
    // The shards really split the data: no node holds everything.
    let rows: Vec<u64> = r.shards.iter().map(|s| s.status(Duration::ZERO).unwrap().observations).collect();
    assert!(rows.iter().all(|n| *n > 0 && *n < 180), "{rows:?}");
}

#[test]
fn counts_and_observations_match() {
    let r = rig("counts", 6);
    let f = DetectionFilter { classes: vec!["person".into()], ..Default::default() };
    r.fed.snapshot().unwrap();
    assert_eq!(r.fed.count_detections(&f).unwrap(), r.single.count_detections(&f).unwrap());
    let a = r.single.observations("m4", 0, 20 * S).unwrap();
    let b = r.fed.observations("m4", 0, 20 * S).unwrap();
    assert_eq!(a.len(), 30);
    assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
}

#[test]
fn killed_node_gives_partial_results_and_old_cursors_keep_paging() {
    let r = rig("kill", 6);
    let f = DetectionFilter::default();
    let all: Vec<_> = {
        let (mut out, mut after) = (vec![], None);
        loop {
            let (rows, next) = r.single.detections(&f, &Page { after: after.clone(), limit: 50 }).unwrap();
            out.extend(rows.into_iter().map(|(o, d)| (o.media_id, o.media_time_us, d.detection_id)));
            match next {
                Some(c) => after = Some(c),
                None => break out,
            }
        }
    };
    r.fed.snapshot().unwrap();
    let (page1, c1) = r.fed.detections(&f, &Page { after: None, limit: 10 }).unwrap();
    assert_eq!(page1.len(), 10);
    let c1 = c1.expect("more pages");

    // Kill n1 and read the next page: survivors answer, the node is reported down.
    r.flaky[1].down.store(true, Ordering::SeqCst);
    let _ = take_run();
    let (page2, c2) = r.fed.detections(&f, &Page { after: Some(c1.clone()), limit: 10 }).unwrap();
    let rep = take_run().report().unwrap();
    assert!(rep.partial && rep.answered == 2 && rep.total == 3, "{rep:?}");
    assert!(rep.nodes.iter().any(|n| n["id"] == "n1" && n["state"] == "down"));
    assert!(page2.iter().all(|(o, _)| o.media_id != "m1" && o.media_id != "m4"), "no rows from the killed node's recordings");
    assert!(c2.is_some());

    // The cursor from before the kill still pages once the node is back, and the traversal is complete and in order.
    r.flaky[1].down.store(false, Ordering::SeqCst);
    let mut got: Vec<_> = page1.iter().map(|(o, d)| (o.media_id.clone(), o.media_time_us, d.detection_id.clone())).collect();
    let (mut after, mut guard) = (Some(c1), 0);
    while let Some(c) = after {
        let (rows, next) = r.fed.detections(&f, &Page { after: Some(c), limit: 10 }).unwrap();
        got.extend(rows.into_iter().map(|(o, d)| (o.media_id, o.media_time_us, d.detection_id)));
        after = next;
        guard += 1;
        assert!(guard < 200);
    }
    assert_eq!(got, all);
    assert!(!take_run().report().map_or(false, |r| r.partial));
}

#[test]
fn query_with_a_dead_node_is_flagged_and_keeps_survivor_rows() {
    let r = rig("dead", 6);
    r.flaky[2].down.store(true, Ordering::SeqCst);
    let rq = req(None, None, Some("person"));
    let o = q::run_detailed(&r.fed, &rq).unwrap();
    let rep = take_run().report().unwrap();
    assert!(rep.partial && rep.answered == 2, "{rep:?}");
    let want = q::run_detailed(&*r.single, &rq).unwrap();
    assert!(o.matches.len() < want.matches.len() && !o.matches.is_empty());
    assert!(o.matches.iter().all(|m| m.media_id != "m2" && m.media_id != "m5"));
}

#[test]
fn a_run_reads_one_snapshot_across_pages() {
    let r = rig("snap", 3);
    let f = DetectionFilter { classes: vec!["person".into()], ..Default::default() };
    let v = r.fed.snapshot().unwrap();
    let (p1, c1) = r.fed.detections(&f, &Page { after: None, limit: 20 }).unwrap();
    // A node commits more while the traversal is in progress.
    let extra: Vec<IndexBatch> = batches(0).into_iter().take(1).map(|mut b| {
        b.observation.media_time_us = 99 * S;
        b.observation.observation_id = observation_id("fp0", 99 * S, "a@1");
        for d in &mut b.detections {
            d.observation_id = b.observation.observation_id.clone();
            d.detection_id = detection_id(&d.observation_id, "m@1", 7);
        }
        b.relations.clear();
        b
    }).collect();
    r.shards[0].commit(&extra[0]).unwrap();
    assert!(r.fed.snapshot().unwrap() > v, "the new commit moves the scalar snapshot");
    let _ = take_run();
    let mut n = p1.len();
    let mut after = c1;
    while let Some(c) = after {
        let (rows, next) = r.fed.detections(&f, &Page { after: Some(c), limit: 20 }).unwrap();
        assert!(rows.iter().all(|(o, _)| o.media_time_us != 99 * S), "later pages stay on the snapshot of page 1");
        n += rows.len();
        after = next;
    }
    assert_eq!(n as u64, r.single.count_detections(&f).unwrap());
}
