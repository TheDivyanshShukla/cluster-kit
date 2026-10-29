use super::*;
use std::sync::Mutex;

const S: Us = 1_000_000;

#[derive(Default)]
struct Fake {
    models: Vec<ModelVersion>,
    media: Vec<MediaAsset>,
    jobs: Vec<IndexJob>,
    rows: Vec<(FrameObservation, Detection)>,
    rels: Vec<Relation>,
    sums: Vec<FrameSummary>,
    runs: Mutex<HashMap<String, QueryRun>>,
    saved: Mutex<HashMap<String, Vec<ReviewMatch>>>,
}

fn pg<T: Clone>(v: Vec<T>, p: &Page) -> StoreResult<(Vec<T>, Option<String>)> {
    let off = p.after.as_ref().map(|a| a.parse::<usize>().unwrap()).unwrap_or(0).min(v.len());
    let end = (off + p.limit as usize).min(v.len());
    Ok((v[off..end].to_vec(), (end < v.len()).then(|| end.to_string())))
}
fn okf(f: &DetectionFilter, o: &FrameObservation, d: Option<&Detection>) -> bool {
    (f.source_ids.is_empty() || f.source_ids.contains(&o.source_id))
        && (f.media_ids.is_empty() || f.media_ids.contains(&o.media_id))
        && f.media_from_us.is_none_or(|t| o.media_time_us >= t)
        && f.media_to_us.is_none_or(|t| o.media_time_us <= t)
        && d.is_none_or(|d| {
            (f.classes.is_empty() || f.classes.contains(&d.class))
                && (f.model_versions.is_empty() || f.model_versions.contains(&d.model_version))
                && f.min_confidence.is_none_or(|c| d.confidence >= c)
                && (f.track_ids.is_empty() || d.track_id.as_ref().is_some_and(|t| f.track_ids.contains(t)))
        })
}
fn nope<T>() -> StoreResult<T> {
    Err(ApiError::new("unavailable", "fake"))
}

impl Store for Fake {
    fn upsert_source(&self, _: &Source) -> StoreResult<()> { Ok(()) }
    fn sources(&self) -> StoreResult<Vec<Source>> { Ok(vec![]) }
    fn upsert_media(&self, _: &MediaAsset) -> StoreResult<()> { Ok(()) }
    fn media(&self, id: &str) -> StoreResult<Option<MediaAsset>> { Ok(self.media.iter().find(|m| m.id == id).cloned()) }
    fn media_for_source(&self, s: &str) -> StoreResult<Vec<MediaAsset>> { Ok(self.media.iter().filter(|m| m.source_id == s).cloned().collect()) }
    fn upsert_model(&self, _: &ModelVersion) -> StoreResult<()> { Ok(()) }
    fn models(&self) -> StoreResult<Vec<ModelVersion>> { Ok(self.models.clone()) }
    fn upsert_job(&self, _: &IndexJob) -> StoreResult<()> { Ok(()) }
    fn job(&self, id: &str) -> StoreResult<Option<IndexJob>> { Ok(self.jobs.iter().find(|j| j.id == id).cloned()) }
    fn jobs(&self) -> StoreResult<Vec<IndexJob>> { Ok(self.jobs.clone()) }
    fn commit_batch(&self, _: &IndexBatch) -> StoreResult<BatchOutcome> { nope() }
    fn detections(&self, f: &DetectionFilter, p: &Page) -> StoreResult<(Vec<(FrameObservation, Detection)>, Option<String>)> {
        let mut v: Vec<_> = self.rows.iter().filter(|(o, d)| okf(f, o, Some(d))).cloned().collect();
        v.sort_by(|a, b| (&a.0.media_id, a.0.media_time_us, &a.1.detection_id).cmp(&(&b.0.media_id, b.0.media_time_us, &b.1.detection_id)));
        pg(v, p)
    }
    fn count_detections(&self, f: &DetectionFilter) -> StoreResult<u64> { Ok(self.rows.iter().filter(|(o, d)| okf(f, o, Some(d))).count() as u64) }
    fn observations(&self, _: &str, _: Us, _: Us) -> StoreResult<Vec<(FrameObservation, Vec<Detection>, Vec<Relation>)>> { Ok(vec![]) }
    fn summaries(&self, f: &DetectionFilter, p: &Page) -> StoreResult<(Vec<FrameSummary>, Option<String>)> {
        let mut v: Vec<_> = self
            .sums
            .iter()
            .filter(|s| (f.source_ids.is_empty() || f.source_ids.contains(&s.source_id)) && (f.media_ids.is_empty() || f.media_ids.contains(&s.media_id)))
            .cloned()
            .collect();
        v.sort_by(|a, b| (&a.media_id, a.media_time_us).cmp(&(&b.media_id, b.media_time_us)));
        pg(v, p)
    }
    fn relations(&self, _: &DetectionFilter, pr: Option<Predicate>, p: &Page) -> StoreResult<(Vec<Relation>, Option<String>)> {
        pg(self.rels.iter().filter(|r| pr.is_none_or(|x| x == r.predicate)).cloned().collect(), p)
    }
    fn save_query_run(&self, q: &QueryRun, m: &[ReviewMatch]) -> StoreResult<()> {
        self.runs.lock().unwrap().insert(q.id.clone(), q.clone());
        self.saved.lock().unwrap().insert(q.id.clone(), m.to_vec());
        Ok(())
    }
    fn query_run(&self, id: &str) -> StoreResult<Option<QueryRun>> { Ok(self.runs.lock().unwrap().get(id).cloned()) }
    fn matches(&self, id: &str, p: &Page) -> StoreResult<(Vec<ReviewMatch>, Option<String>)> {
        let mut v = self.saved.lock().unwrap().get(id).cloned().unwrap_or_default();
        v.sort_by(|a, b| (&a.media_id, a.start_us, &a.id).cmp(&(&b.media_id, b.start_us, &b.id)));
        pg(v, p)
    }
    fn save_review(&self, _: &ReviewSelection, _: Option<u64>) -> StoreResult<ReviewSelection> { nope() }
    fn review(&self, _: &str) -> StoreResult<Option<ReviewSelection>> { Ok(None) }
    fn reviews(&self) -> StoreResult<Vec<ReviewSelection>> { Ok(vec![]) }
    fn upsert_alert(&self, _: &Alert) -> StoreResult<()> { Ok(()) }
    fn alerts(&self, _: &Page) -> StoreResult<(Vec<Alert>, Option<String>)> { Ok((vec![], None)) }
    fn audit(&self, _: &AuditRecord) -> StoreResult<()> { Ok(()) }
    fn audit_log(&self, _: Option<&str>, _: &Page) -> StoreResult<(Vec<AuditRecord>, Option<String>)> { Ok((vec![], None)) }
    fn snapshot(&self) -> StoreResult<u64> { Ok(7) }
}

fn asset(id: &str, dur: Option<Us>, utc: Option<Us>) -> MediaAsset {
    MediaAsset {
        id: id.into(), source_id: "cam".into(), uri: format!("/v/{id}.mp4"), fingerprint: "f".into(), duration_us: dur,
        width: None, height: None, codec: None, timebase: None, capture_start_utc_us: utc,
        proxy: ProxyStatus::None, availability: Availability::Available, still: false, node_id: None,
    }
}
pub(super) fn row(media: &str, t: Us, id: &str, class: &str, conf: f32, track: Option<&str>, model: &str) -> (FrameObservation, Detection) {
    let oid = format!("obs_{media}_{t}");
    (
        FrameObservation {
            observation_id: oid.clone(), source_id: "cam".into(), media_id: media.into(), job_id: "j".into(), frame_index: None,
            pts: None, timebase: None, media_time_us: t, capture_time_utc_us: None, indexed_at_utc_us: 0,
            model_versions: vec![], quality: Quality::Ok, thumbnail: None,
        },
        Detection {
            detection_id: format!("det_{id}"), observation_id: oid, class: class.into(), confidence: conf, bbox: [0.0; 4],
            model_version: model.into(), track_id: track.map(Into::into), provenance: "model".into(), color: None,
        },
    )
}
fn model(holding: bool) -> ModelVersion {
    ModelVersion {
        id: "rel".into(), version: "1".into(), artifact_hash: "h".into(), adapter: AdapterKind::Relation, output_schema: "x".into(),
        labels: vec!["person".into(), "cell phone".into()],
        supported_relations: if holding { vec![Predicate::Holding] } else { vec![Predicate::Near] },
        thresholds: Default::default(), provider: "cpu".into(), input_size: None, validation: "unvalidated".into(),
    }
}
fn sum(t: Us, people: Option<u32>, holding: Option<u32>) -> FrameSummary {
    let mut class_counts = BTreeMap::new();
    if let Some(p) = people {
        class_counts.insert("person".to_string(), Count::Known(p));
    }
    let mut relation_counts = BTreeMap::new();
    if let Some(h) = holding {
        relation_counts.insert("holding:cell phone".to_string(), Count::Known(h));
    }
    FrameSummary {
        observation_id: format!("obs_f_{t}"), source_id: "cam".into(), media_id: "a".into(), media_time_us: t, capture_time_utc_us: None,
        zone: None, min_confidence: 0.3, class_counts, relation_counts, quality: Quality::Ok,
    }
}
fn req(sql: &str) -> Request {
    Request { sql: Some(sql.into()), lua: None, class: None, min_confidence: None, scope: empty_scope(), gap_us: None, pre_us: None, post_us: None, limit: None, actor: None }
}
fn store_with(rows: Vec<(FrameObservation, Detection)>) -> Fake {
    Fake { media: vec![asset("a", Some(60 * S), None)], rows, ..Default::default() }
}
fn code(r: Result<Outcome, ApiError>) -> String {
    r.err().map(|e| e.code).unwrap_or_default()
}

#[test]
fn frames_60_people_12_holders_is_twenty_percent() {
    let mut st = Fake { media: vec![asset("a", Some(60 * S), None)], models: vec![model(true)], ..Default::default() };
    st.sums = vec![sum(S, Some(60), Some(12)), sum(2 * S, Some(60), Some(5)), sum(3 * S, None, Some(3))];
    let sql = "SELECT * FROM frames WHERE people_count > 0 AND phone_holders_count >= 4 AND phone_holders_count * 1.0 / people_count >= 0.2";
    let o = run_detailed(&st, &req(sql)).unwrap();
    assert_eq!((o.run.result_count, o.run.kind, o.unit.as_str()), (1, ResultKind::Frames, "frames"));
    assert_eq!(o.matches.len(), 1);
    assert_eq!(o.matches[0].start_us, S);
    assert!(o.run.warnings.iter().any(|w| w.contains("unknown counts")), "{:?}", o.run.warnings);
    // Unknown never satisfies numeric predicates, even a negated one.
    let o = run_detailed(&st, &req("SELECT COUNT(*) FROM frames WHERE NOT people_count > 100")).unwrap();
    assert_eq!(o.run.result_count, 2);
    // Without a Holding-capable model the same query is unsupported and names what is needed.
    st.models = vec![model(false)];
    let e = run_detailed(&st, &req(sql)).err().unwrap();
    assert_eq!(e.code, "unsupported");
    assert!(e.message.contains("Holding"), "{}", e.message);
    // Counting people does not need Holding.
    assert_eq!(run_detailed(&st, &req("SELECT COUNT(*) FROM frames WHERE people_count >= 60 AND count('person') = 60")).unwrap().run.result_count, 2);
}

#[test]
fn holding_relations_need_a_capable_model_and_count_tracks_once() {
    let mk = |id: &str, t: &str| Relation {
        relation_id: format!("rel_{id}"), observation_id: format!("obs_a_{}", id), subject_detection_id: format!("det_{id}"),
        subject_track_id: Some(t.into()), predicate: Predicate::Holding, object_detection_id: "o".into(), object_track_id: None,
        object_class: "cell phone".into(), confidence: 0.7, module_version: "rel@1".into(), method: "pose".into(),
    };
    let mut st = store_with(vec![row("a", 1, "1", "person", 0.9, Some("t1"), "m"), row("a", 2, "2", "person", 0.9, Some("t1"), "m")]);
    st.rels = vec![mk("1", "t1"), mk("2", "t1")];
    let sql = "SELECT DISTINCT subject_track_id FROM relations WHERE predicate = 'holding' AND object_class = 'cell phone' LIMIT 5";
    assert_eq!(code(run_detailed(&st, &req(sql))), "unsupported");
    st.models = vec![model(true)];
    let o = run_detailed(&st, &req(sql)).unwrap();
    assert_eq!((o.values.len(), o.run.result_count, o.unit.as_str()), (1, 1, "distinct_tracks"));
    let o = run_detailed(&st, &req("SELECT * FROM relations WHERE predicate = 'holding'")).unwrap();
    assert_eq!((o.run.result_count, o.matches.len(), o.matches[0].distinct_tracks), (2, 1, 1));
    assert_eq!(o.matches[0].relation_ids.len(), 2);
}

#[test]
fn five_observations_of_one_track_are_one_distinct_track() {
    let st = store_with((0..5).map(|i| row("a", i * S, &i.to_string(), "cell phone", 0.9, Some("t1"), "m")).collect());
    let o = run_detailed(&st, &req("SELECT COUNT(*) FROM detections WHERE class = 'cell phone'")).unwrap();
    assert_eq!((o.run.result_count, o.unit.as_str()), (5, "observations"));
    assert!(o.run.warnings.iter().any(|w| w == "unit: observations"));
    let o = run_detailed(&st, &req("SELECT COUNT(DISTINCT track_id) FROM sightings WHERE class = 'cell phone'")).unwrap();
    assert_eq!((o.run.result_count, o.unit.as_str()), (1, "distinct_tracks"));
    let o = run_detailed(&st, &req("SELECT * FROM detections")).unwrap();
    assert_eq!((o.run.result_count, o.distinct_tracks, o.matches.len(), o.matches[0].distinct_tracks), (5, Some(1), 1, 1));
    let o = run_detailed(&st, &req("SELECT * FROM tracks WHERE observations >= 5")).unwrap();
    assert_eq!((o.run.result_count, o.unit.as_str(), o.matches.len()), (1, "tracks", 1));
}

#[test]
fn gaps_merge_and_split_through_the_request() {
    let st = store_with(vec![row("a", 10 * S, "1", "person", 0.9, None, "m"), row("a", 11 * S, "2", "person", 0.9, None, "m"), row("a", 20 * S, "3", "person", 0.9, None, "m")]);
    assert_eq!(run(&st, &req("SELECT * FROM detections")).unwrap().1.len(), 2);
    let mut r = req("SELECT * FROM detections");
    r.gap_us = Some(100);
    assert_eq!(run(&st, &r).unwrap().1.len(), 3);
    r.gap_us = Some(20 * S);
    assert_eq!(run(&st, &r).unwrap().1.len(), 1);
}

#[test]
fn confidence_and_model_filters() {
    let st = store_with(vec![
        row("a", S, "1", "person", 0.5, None, "m1"),
        row("a", 2 * S, "2", "person", 0.85, None, "m1"),
        row("a", 3 * S, "3", "person", 0.95, None, "m2"),
    ]);
    let n = |sql: &str| run_detailed(&st, &req(sql)).unwrap().run.result_count;
    assert_eq!(n("SELECT COUNT(*) FROM detections WHERE confidence >= 0.8"), 2);
    assert_eq!(n("SELECT COUNT(*) FROM detections WHERE confidence <= 0.5"), 1);
    assert_eq!(n("SELECT COUNT(*) FROM detections WHERE confidence >= 0.8 AND model_version = 'm2'"), 1);
    assert_eq!(n("SELECT COUNT(*) FROM detections WHERE model_version IN ('m1','m2') AND NOT class = 'dog'"), 3);
    let mut r = req("SELECT * FROM detections WHERE model_version = 'm1'");
    r.min_confidence = Some(0.8);
    assert_eq!(run_detailed(&st, &r).unwrap().run.result_count, 1);
    // Request-only form.
    let r = Request { sql: None, class: Some("person".into()), min_confidence: Some(0.9), ..req("") };
    assert_eq!(run_detailed(&st, &r).unwrap().run.result_count, 1);
    assert_eq!(code(run_detailed(&st, &Request { sql: None, ..req("") })), "invalid_request");
    // ORDER BY and LIMIT.
    let o = run_detailed(&st, &req("SELECT * FROM detections ORDER BY media_time_us DESC LIMIT 2")).unwrap();
    assert_eq!(o.run.result_count, 2);
    assert!(o.run.warnings.iter().any(|w| w.contains("limited")));
    assert_eq!(o.matches[0].start_us, 2 * S);
}

#[test]
fn scope_and_contradictions() {
    let mut st = store_with(vec![row("a", S, "1", "person", 0.9, None, "m")]);
    st.rows.push(row("b", S, "2", "person", 0.9, None, "m"));
    let mut r = req("SELECT COUNT(*) FROM detections");
    r.scope.media_ids = vec!["b".into()];
    assert_eq!(run_detailed(&st, &r).unwrap().run.result_count, 1);
    r.sql = Some("SELECT COUNT(*) FROM detections WHERE media_id = 'a'".into());
    let o = run_detailed(&st, &r).unwrap();
    assert_eq!(o.run.result_count, 0);
    assert!(o.run.warnings.iter().any(|w| w.contains("not evidence of absence")));
}

#[test]
fn zero_results_with_partial_coverage_warn() {
    let st = store_with(vec![]);
    let o = run_detailed(&st, &req("SELECT * FROM detections WHERE class = 'person'")).unwrap();
    assert!(o.run.warnings.iter().any(|w| w.contains("zero results are not evidence of absence")));
}

#[test]
fn parser_errors_have_positions_and_budgets() {
    let st = store_with(vec![]);
    let e = run_detailed(&st, &req("SELECT * FROM detections WHERE bogus = 1")).err().unwrap();
    assert_eq!(e.code, "parse_error");
    assert!(e.message.contains("unknown field 'bogus'") && e.message.contains("position 31"), "{}", e.message);
    assert_eq!(code(run_detailed(&st, &req("SELECT * FROM nothing"))), "parse_error");
    assert_eq!(code(run_detailed(&st, &req("SELECT * FROM events"))), "unsupported");
    assert_eq!(code(run_detailed(&st, &req("SELECT * FROM detections; DELETE FROM detections"))), "parse_error");
    assert_eq!(code(run_detailed(&st, &req("SELECT * FROM detections WHERE confidence = 'x'"))), "parse_error");
    assert_eq!(code(run_detailed(&st, &req("SELECT * FROM detections WHERE class = 'a' OR"))), "parse_error");
    assert_eq!(code(run_detailed(&st, &req("SELECT * FROM detections WHERE phone_holders_count > 1"))), "parse_error");
    assert_eq!(code(run_detailed(&st, &req("SELECT * FROM relations ORDER BY media_time_us"))), "parse_error");
    assert_eq!(code(run_detailed(&st, &req("SELECT * FROM detections LIMIT 1.5"))), "parse_error");
    // Budgets.
    assert_eq!(code(run_detailed(&st, &req(&" ".repeat(MAX_SQL_LEN + 1)))), "too_large");
    let nested = format!("SELECT * FROM detections WHERE {}class = 'person'{}", "(".repeat(26), ")".repeat(26));
    assert_eq!(code(run_detailed(&st, &req(&nested))), "too_large");
    let list = (0..300).map(|i| format!("'c{i}'")).collect::<Vec<_>>().join(",");
    assert_eq!(code(run_detailed(&st, &req(&format!("SELECT * FROM detections WHERE class IN ({list})")))), "too_large");
    let wide = format!("SELECT * FROM detections WHERE {}class = 'person'", "class = 'person' OR ".repeat(300));
    assert_eq!(code(run_detailed(&st, &req(&wide))), "too_large");
    let nots = format!("SELECT * FROM detections WHERE {} class = 'x'", "NOT ".repeat(30));
    assert_eq!(code(run_detailed(&st, &req(&nots))), "too_large");
}

#[test]
fn retry_is_idempotent_and_run_ids_are_stable() {
    let mut rows = vec![row("a", S, "1", "person", 0.9, None, "m"), row("a", 30 * S, "2", "person", 0.9, None, "m")];
    let st = store_with(rows.clone());
    let a = run(&st, &req("SELECT * FROM detections")).unwrap();
    let b = run(&st, &req("select  *  from SIGHTINGS")).unwrap();
    assert_eq!(a.0.id, b.0.id);
    assert_eq!(a.1.iter().map(|m| &m.id).collect::<Vec<_>>(), b.1.iter().map(|m| &m.id).collect::<Vec<_>>());
    assert_eq!(st.saved.lock().unwrap()[&a.0.id].len(), 2);
    // A retried batch delivering the same rows twice does not raise window counts.
    rows.extend(rows.clone());
    let st2 = store_with(rows);
    assert_eq!(run(&st2, &req("SELECT * FROM detections")).unwrap().1.len(), 2);
}

#[test]
fn cursor_pages_are_complete_and_stable() {
    let st = store_with((0..7).map(|i| row("a", i * 10 * S, &i.to_string(), "person", 0.9, None, "m")).collect());
    let (run, ms) = run(&st, &req("SELECT * FROM detections")).unwrap();
    assert_eq!(ms.len(), 7);
    let mut seen = Vec::new();
    let mut cur = None;
    loop {
        let (page, next) = matches_page(&st, &run.id, cur, 3).unwrap();
        assert!(page.len() <= 3);
        seen.extend(page.into_iter().map(|m| m.id));
        match next {
            Some(c) => cur = Some(c),
            None => break,
        }
    }
    let uniq: HashSet<_> = seen.iter().collect();
    assert_eq!((seen.len(), uniq.len()), (7, 7));
    assert_eq!(matches_page(&st, "missing", None, 3).err().unwrap().code, "not_found");
}

#[test]
fn bounded_scan_returns_partial_with_warning() {
    let st = store_with((0..10).map(|i| row("a", i * S, &i.to_string(), "person", 0.9, None, "m")).collect());
    let lim = Limits { max_scan: 3, page: 2, ..Default::default() };
    let o = run_with(&st, &req("SELECT COUNT(*) FROM detections"), &lim).unwrap();
    assert_eq!(o.run.result_count, 4);
    assert!(o.run.warnings.iter().any(|w| w.starts_with("partial results")), "{:?}", o.run.warnings);
    let lim = Limits { deadline: Duration::ZERO, ..Default::default() };
    let o = run_with(&st, &req("SELECT * FROM detections"), &lim).unwrap();
    assert_eq!(o.run.result_count, 0);
    assert!(o.run.warnings.iter().any(|w| w.contains("deadline") ) && o.run.warnings.iter().any(|w| w.contains("not evidence of absence")));
}

#[test]
fn timeline_lanes_coalescing_and_gaps() {
    let mut st = Fake {
        media: vec![asset("a", Some(60 * S), Some(1_700_000_000 * S)), asset("b", Some(30 * S), None)],
        rows: vec![
            row("a", 10 * S, "1", "person", 0.9, None, "m"),
            row("a", 13 * S, "2", "person", 0.9, None, "m"),
            row("a", 40 * S, "3", "person", 0.9, None, "m"),
            row("b", 5 * S, "4", "person", 0.9, None, "m"),
        ],
        ..Default::default()
    };
    st.jobs = vec![IndexJob {
        id: "j".into(), manifest_version: "1".into(), media_ids: vec!["a".into()], model_versions: vec![], sampling: json!({}),
        state: JobState::Completed, checkpoint: Default::default(),
        coverage: Coverage { decoded: 10, inferred: 10, skipped: 0, failed: 0, analyzed: vec![[0, 20 * S], [30 * S, 60 * S]] },
        error: None, created_utc_us: 0, finished_utc_us: None,
    }];
    let (run, ms) = run(&st, &req("SELECT * FROM detections")).unwrap();
    assert_eq!(ms.len(), 4);
    let t = timeline(&st, &run.id, 0, 60 * S, 0).unwrap();
    let lanes = t["lanes"].as_array().unwrap();
    assert_eq!(lanes.len(), 2);
    assert_eq!(lanes[0]["media_id"], "a");
    assert_eq!(lanes[0]["clock"]["kind"], "utc");
    assert_eq!(lanes[1]["clock"]["kind"], "media");
    assert_eq!(lanes[0]["matches"].as_array().unwrap().len(), 3);
    assert_eq!(lanes[0]["capture_start_utc_us"], 1_700_000_000 * S);
    assert_eq!(lanes[0]["coverage_known"], true);
    assert_eq!(lanes[1]["coverage_known"], false);
    let gaps = t["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 1);
    assert_eq!((gaps[0]["start_us"].as_i64(), gaps[0]["end_us"].as_i64()), (Some(20 * S), Some(30 * S)));
    // Coarse resolution coalesces the two near windows but reports both as exact.
    let t = timeline(&st, &run.id, 0, 60 * S, 5 * S).unwrap();
    let m = t["lanes"][0]["matches"].as_array().unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!((m[0]["exact"].as_u64(), m[0]["coalesced"].as_bool()), (Some(2), Some(true)));
    assert_eq!(t["lanes"][0]["exact_count"], 3);
    // Visible range excludes far windows.
    let t = timeline(&st, &run.id, 35 * S, 50 * S, 0).unwrap();
    assert_eq!(t["lanes"][0]["matches"].as_array().unwrap().len(), 1);
    assert_eq!(timeline(&st, &run.id, 5, 1, 0).err().unwrap().code, "invalid_request");
}

#[test]
fn coverage_is_aggregated_from_jobs() {
    let mut st = store_with(vec![]);
    st.jobs = vec![IndexJob {
        id: "j".into(), manifest_version: "1".into(), media_ids: vec!["a".into()], model_versions: vec![], sampling: json!({}),
        state: JobState::Completed, checkpoint: Default::default(),
        coverage: Coverage { decoded: 100, inferred: 40, skipped: 60, failed: 0, analyzed: vec![[0, S]] },
        error: None, created_utc_us: 0, finished_utc_us: None,
    }];
    let mut r = req("SELECT * FROM detections");
    r.scope.media_ids = vec!["a".into()];
    let o = run_detailed(&st, &r).unwrap();
    assert_eq!((o.run.coverage.decoded, o.run.coverage.inferred, o.run.coverage.analyzed.len()), (100, 40, 1));
    assert!(o.run.warnings.iter().any(|w| w.contains("not evidence of absence")));
}

fn lreq(src: &str) -> Request {
    Request { sql: None, lua: Some(src.into()), ..req("") }
}

#[test]
fn lua_loop_over_frames_computes_ratio() {
    let mut st = Fake { media: vec![asset("a", Some(60 * S), None)], models: vec![model(true)], ..Default::default() };
    st.sums = vec![sum(S, Some(60), Some(12)), sum(2 * S, Some(60), Some(5)), sum(3 * S, None, Some(3))];
    let o = run_detailed(&st, &lreq("for f in db.frames{} do local p, h = f.counts.person, f.relations['holding:cell phone']\n if p and h and p > 0 and h / p >= 0.2 then emit(f) end end")).unwrap();
    assert_eq!((o.run.result_count, o.matches.len(), o.run.kind), (1, 1, ResultKind::Frames));
    assert!(o.run.normalized.starts_with("lua:"));
    // function(db) form, returning an aggregate table.
    let o = run_detailed(&st, &lreq("return function(db) return {frames = count(db.frames{}), people = distinct(collect(db.frames{}), 'media_id')} end")).unwrap();
    assert_eq!(o.values[0]["frames"], 3);
    assert_eq!(o.run.kind, ResultKind::Aggregate);
}

#[test]
fn lua_script_without_sources_keeps_the_search_scope() {
    // Regression: a Lua call that names no source must not empty a scope that names one.
    let mut st = Fake { media: vec![asset("a", Some(60 * S), None)], models: vec![model(true)], ..Default::default() };
    st.sums = vec![sum(S, Some(60), Some(12)), sum(2 * S, Some(60), Some(5))];
    let mut r = lreq("return {frames = count(db.frames{})}");
    r.scope.source_ids = vec!["cam".into()];
    let scoped = run_detailed(&st, &r).unwrap();
    let open = run_detailed(&st, &lreq("return {frames = count(db.frames{})}")).unwrap();
    assert_eq!(scoped.values[0]["frames"], open.values[0]["frames"]);
    assert!(scoped.values[0]["frames"].as_u64().unwrap() > 0);
}

#[test]
fn lua_nested_loops_emit_and_same_script_same_run() {
    let st = store_with(vec![
        row("a", S, "1", "person", 0.9, Some("t1"), "m"),
        row("a", S, "2", "cell phone", 0.8, None, "m"),
        row("a", 20 * S, "3", "person", 0.9, Some("t1"), "m"),
    ]);
    let src = "local phones = collect(db.detections{class='cell phone'})\nfor p in db.detections{class='person'} do for _, ph in ipairs(phones) do if ph.observation_id == p.observation_id then emit(p) emit(ph) end end end";
    let o = run_detailed(&st, &lreq(src)).unwrap();
    assert_eq!((o.matches.len(), o.run.result_count, o.matches[0].detection_ids.len()), (1, 2, 2));
    let o2 = run_detailed(&st, &lreq(src)).unwrap();
    assert_eq!(o.run.id, o2.run.id);
    assert_eq!(emit_by_id(&st), 1);
}
fn emit_by_id(st: &Fake) -> usize {
    let o = run_detailed(st, &lreq("emit('obs_a_1000000')\nfor r in db.detections{} do end emit('obs_a_1000000')")).unwrap();
    o.matches.len()
}

#[test]
fn lua_budgets_and_sandbox() {
    let st = store_with(vec![row("a", S, "1", "person", 0.9, None, "m")]);
    let lim = Limits { deadline: Duration::from_millis(300), ..Default::default() };
    let o = run_with(&st, &lreq("while true do end"), &lim).unwrap();
    assert!(o.run.warnings.iter().any(|w| w.contains("budget")), "{:?}", o.run.warnings);
    let e = run_detailed(&st, &lreq("local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 1000) .. i end")).err().unwrap();
    assert_eq!(e.code, "runtime_error");
    assert!(e.message.contains("memory"), "{}", e.message);
    let o = run_detailed(&st, &lreq("return {io == nil, os == nil, load == nil, require == nil, debug == nil, package == nil}")).unwrap();
    assert_eq!(o.values, vec![json!(true); 6]);
    let e = run_detailed(&st, &lreq("local x = 1\nos.execute('true')")).err().unwrap();
    assert!(e.code == "runtime_error" && e.message.contains("lua:2"), "{}", e.message);
    assert_eq!(run_detailed(&st, &lreq("for do")).err().unwrap().code, "parse_error");
    let mut both = lreq("return 1");
    both.sql = Some("SELECT * FROM detections".into());
    assert_eq!(code(run_detailed(&st, &both)), "invalid_request");
}

#[test]
fn lua_pages_across_store_pages_and_holding_is_unsupported() {
    let mut st = store_with((0..10).map(|i| row("a", i * S, &i.to_string(), "person", 0.9, None, "m")).collect());
    let lim = Limits { page: 3, ..Default::default() };
    let o = run_with(&st, &lreq("return count(db.detections{class='person'})"), &lim).unwrap();
    assert_eq!((o.run.result_count, o.values[0].clone()), (10, json!(10)));
    let lim = Limits { page: 3, max_scan: 5, ..Default::default() };
    let o = run_with(&st, &lreq("return count(db.detections{})"), &lim).unwrap();
    assert!(o.run.warnings.iter().any(|w| w.starts_with("partial results")));
    assert_eq!(code(run_detailed(&st, &lreq("for r in db.relations{predicate='holding'} do emit(r) end"))), "unsupported");
    st.models = vec![model(true)];
    assert!(run_detailed(&st, &lreq("for r in db.relations{predicate='holding'} do emit(r) end")).is_ok());
}
